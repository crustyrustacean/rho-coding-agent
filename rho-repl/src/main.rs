//! rho-repl — thin terminal client for rho.
//!
//! Spawns rho as a subprocess and communicates via JSON-RPC 2.0 over
//! stdin/stdout. Provides an interactive terminal experience with slash
//! commands, streaming output, approval prompts, and command history.
//!
//! # Usage
//!
//! ```sh
//! rho-repl --model qwen3-8b
//! rho-repl --cargo --model qwen3-8b        # development: uses `cargo run -p rho`
//! rho-repl --rho-path ./target/debug/rho     # explicit binary path
//! ```

mod client;
mod render;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use colored::Colorize;
use render::Renderer;
use rustyline::error::ReadlineError;
use serde_json::{Value, json};
use std::io::Write;

// ── CLI ────────────────────────────────────────────────────────────────────────

/// rho-repl — interactive terminal client for rho.
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Path to the rho binary (default: `rho` from PATH).
    #[arg(long)]
    rho_path: Option<String>,

    /// Use `cargo run -p rho` to launch rho (for development).
    #[arg(long)]
    cargo: bool,

    /// Model identifier (forwarded to rho).
    #[arg(short, long)]
    model: Option<String>,

    /// System prompt override (forwarded to rho).
    #[arg(short, long)]
    system: Option<String>,

    /// Use compact system prompt (forwarded to rho).
    #[arg(long)]
    compact: bool,

    /// Project / sandbox root (forwarded to rho).
    #[arg(long)]
    root: Option<String>,

    /// Model API endpoint URL (forwarded to rho).
    #[arg(long)]
    endpoint: Option<String>,

    /// Skip provider consent warning (forwarded to rho).
    #[arg(long)]
    accept_external_provider: bool,

    /// Environment variable holding the API key (forwarded to rho).
    #[arg(long)]
    api_key_env: Option<String>,

    /// Maximum agent loop iterations (forwarded to rho).
    #[arg(long)]
    max_iterations: Option<u32>,

    /// Context window token budget (forwarded to rho).
    #[arg(long)]
    token_budget: Option<u32>,

    /// Resume most recent session (forwarded to rho).
    #[arg(short = 'c', long)]
    r#continue: bool,

    /// Resume session from a JSONL file (forwarded to rho).
    #[arg(long)]
    session: Option<String>,

    /// Ephemeral mode — no session persistence (forwarded to rho).
    #[arg(long)]
    ephemeral: bool,
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// Build the command-line argument vector for the rho subprocess.
///
/// When `--cargo` is set, prefixes `cargo run -p rho --` before the
/// forwarded arguments. Otherwise uses `--rho-path` or `"rho"`.
fn build_rho_args(cli: &Cli) -> Vec<String> {
    let mut args = Vec::new();

    if cli.cargo {
        args.extend_from_slice(&[
            "cargo".to_owned(),
            "run".to_owned(),
            "-p".to_owned(),
            "rho".to_owned(),
            "--".to_owned(),
        ]);
    } else {
        args.push(cli.rho_path.clone().unwrap_or_else(|| "rho".to_owned()));
    }

    let flag = |args: &mut Vec<String>, name: &str| args.push(name.to_owned());
    let valued = |args: &mut Vec<String>, name: &str, val: &str| {
        args.push(name.to_owned());
        args.push(val.to_owned());
    };

    if let Some(ref v) = cli.model {
        valued(&mut args, "--model", v);
    }
    if let Some(ref v) = cli.system {
        valued(&mut args, "--system", v);
    }
    if cli.compact {
        flag(&mut args, "--compact");
    }
    if let Some(ref v) = cli.root {
        valued(&mut args, "--root", v);
    }
    if let Some(ref v) = cli.endpoint {
        valued(&mut args, "--endpoint", v);
    }
    if cli.accept_external_provider {
        flag(&mut args, "--accept-external-provider");
    }
    if let Some(ref v) = cli.api_key_env {
        valued(&mut args, "--api-key-env", v);
    }
    if let Some(v) = cli.max_iterations {
        valued(&mut args, "--max-iterations", &v.to_string());
    }
    if let Some(v) = cli.token_budget {
        valued(&mut args, "--token-budget", &v.to_string());
    }
    if cli.r#continue {
        flag(&mut args, "--continue");
    }
    if let Some(ref v) = cli.session {
        valued(&mut args, "--session", v);
    }
    if cli.ephemeral {
        flag(&mut args, "--ephemeral");
    }

    args
}

/// Path to the readline history file (`~/.rho/repl-history`).
fn history_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".rho")
            .join("repl-history"),
    )
}

/// Print help text for slash commands.
fn print_help() {
    let lines = [
        ("  /clear", "Clear conversation history"),
        ("  /model <id>", "Switch model (bare id or provider:id)"),
        ("  /models", "List available models"),
        ("  /providers", "List configured providers"),
        ("  /status", "Show context window usage"),
        ("  /sessions", "List previous sessions"),
        ("  /extensions", "List loaded extensions"),
        ("  /reload", "Hot-reload extensions from disk"),
        ("  /compact", "Trigger context compaction"),
        ("  /quit", "Exit"),
    ];
    let max_cmd = lines.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    println!("{}", "Commands:".bold());
    for (cmd, desc) in &lines {
        println!("  {:>width$}  {desc}", cmd, width = max_cmd);
    }
}

/// Prompt the user for approval of a tool call.
///
/// Reads from stdin synchronously. Returns `true` if approved.
fn prompt_approval(params: &Value) -> bool {
    let tool = params
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let risk = params
        .get("risk")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let args = params
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    println!(
        "\n{} {} [{}]",
        "approve?".yellow().bold(),
        tool.yellow(),
        risk.yellow(),
    );

    // Truncate long arguments for display.
    let preview = if args.len() > 120 {
        format!("{}…", &args[..117])
    } else {
        args.to_owned()
    };
    println!("  {}", preview.dimmed());

    eprint!("  {} ", "[y/N]".bold());
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    let answer = answer.trim().to_lowercase();
    answer == "y" || answer == "yes"
}

// ── request helpers ────────────────────────────────────────────────────────────

/// Send a request, then consume messages until the matching response arrives,
/// rendering any notifications along the way.
async fn request_and_render(
    rho: &mut client::RhoClient,
    renderer: &mut Renderer,
    method: &str,
    params: Value,
) -> Result<()> {
    let id = rho.send(method, params).await?;
    wait_for_response(rho, renderer, id, method).await
}

/// Consume messages until the response with the given ID arrives.
///
/// Notifications are rendered immediately. Responses to *other* requests
/// (e.g. `approvalResponse` during a prompt) are silently discarded.
async fn wait_for_response(
    rho: &mut client::RhoClient,
    renderer: &mut Renderer,
    id: u64,
    method: &str,
) -> Result<()> {
    loop {
        match rho.recv().await? {
            client::Message::Response {
                id: resp_id,
                result,
                error,
            } if resp_id == json!(id) => {
                if let Some(err) = &error {
                    let code = err["code"].as_i64().unwrap_or(-1) as i32;
                    let msg = err["message"].as_str().unwrap_or("unknown error");
                    renderer.error_response(code, msg);
                } else if let Some(r) = &result {
                    renderer.response(method, r);
                }
                return Ok(());
            }
            // Response to a different request — discard.
            client::Message::Response { .. } => continue,
            // Notification — render it.
            client::Message::Notification { method: m, params } => {
                renderer.notification(&m, &params);
            }
        }
    }
}

/// Process events during a `prompt` request until the prompt response arrives.
///
/// Handles the approval flow inline: when an `approval/request` notification
/// arrives, prompts the user and sends the `approvalResponse`.
async fn process_prompt(
    rho: &mut client::RhoClient,
    renderer: &mut Renderer,
    prompt_id: u64,
) -> Result<()> {
    loop {
        match rho.recv().await? {
            client::Message::Response {
                id: resp_id,
                result,
                error,
            } if resp_id == json!(prompt_id) => {
                renderer.end_stream();
                if let Some(err) = &error {
                    let msg = err["message"].as_str().unwrap_or("unknown error");
                    renderer.error_response(-1, msg);
                } else if let Some(r) = &result {
                    renderer.response("prompt", r);
                }
                return Ok(());
            }
            // Response to a different request (e.g. approvalResponse) — discard.
            client::Message::Response { .. } => continue,
            client::Message::Notification { method, params } => {
                if method == "approval/request" {
                    let approved = prompt_approval(&params);
                    // Fire-and-forget: the response is consumed by the loop
                    // above and discarded as a "different request" response.
                    rho.send("approvalResponse", json!({"approved": approved}))
                        .await?;
                } else {
                    renderer.notification(&method, &params);
                }
            }
        }
    }
}

// ── slash command dispatch ────────────────────────────────────────────────────

/// Result of handling a user input line.
enum Action {
    /// Continue the main loop (prompt again).
    Continue,
    /// Exit the REPL.
    Quit,
}

/// Handle a single line of user input.
///
/// Slash commands are dispatched to the appropriate JSON-RPC method.
/// Plain text is sent as a `prompt` request.
async fn handle_input(
    input: &str,
    rho: &mut client::RhoClient,
    renderer: &mut Renderer,
) -> Result<Action> {
    ensure!(
        input.starts_with('/'),
        "handle_input expects a slash command"
    );

    let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match cmd {
        "quit" | "exit" => Ok(Action::Quit),

        "help" => {
            print_help();
            Ok(Action::Continue)
        }

        "clear" => {
            request_and_render(rho, renderer, "clear", json!({})).await?;
            Ok(Action::Continue)
        }

        "model" => {
            let model = match arg {
                Some(m) => m,
                None => {
                    eprintln!("{} /model <id>", "usage:".red());
                    return Ok(Action::Continue);
                }
            };
            request_and_render(rho, renderer, "setModel", json!({"model": model})).await?;
            Ok(Action::Continue)
        }

        "models" => {
            request_and_render(rho, renderer, "listModels", json!({})).await?;
            Ok(Action::Continue)
        }

        "providers" => {
            request_and_render(rho, renderer, "listProviders", json!({})).await?;
            Ok(Action::Continue)
        }

        "status" => {
            request_and_render(rho, renderer, "getSessionStats", json!({})).await?;
            Ok(Action::Continue)
        }

        "sessions" => {
            request_and_render(rho, renderer, "listSessions", json!({})).await?;
            Ok(Action::Continue)
        }

        "extensions" => {
            request_and_render(rho, renderer, "listExtensions", json!({})).await?;
            Ok(Action::Continue)
        }

        "reload" => {
            request_and_render(rho, renderer, "reloadExtensions", json!({})).await?;
            Ok(Action::Continue)
        }

        "compact" => {
            request_and_render(rho, renderer, "compact", json!({})).await?;
            Ok(Action::Continue)
        }

        _ => {
            eprintln!(
                "{} unknown command: /{cmd}. Type /help for commands.",
                "error:".red()
            );
            Ok(Action::Continue)
        }
    }
}

// ── main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let rho_args = build_rho_args(&cli);

    // Spawn rho.
    let mut rho = client::RhoClient::spawn(&rho_args)?;
    let mut renderer = Renderer::new();

    // Wait for the `ready` notification.
    match rho.recv().await? {
        client::Message::Notification { method, params } => {
            renderer.notification(&method, &params);
        }
        client::Message::Response {
            error: Some(err), ..
        } => {
            let msg = err["message"].as_str().unwrap_or("unknown error");
            anyhow::bail!("rho failed to start: {msg}");
        }
        _ => anyhow::bail!("unexpected initial message from rho"),
    }

    // Show current model state.
    let state_id = rho.send("getState", json!({})).await?;
    wait_for_response(&mut rho, &mut renderer, state_id, "getState").await?;

    println!(
        "\n  {}",
        "Type a message or /help for commands. Ctrl+C to re-prompt.".dimmed(),
    );
    println!();

    // Set up readline with optional persistent history.
    let mut editor = rustyline::DefaultEditor::new().context("failed to create readline editor")?;

    if let Some(ref path) = history_path()
        && path.exists()
    {
        let _ = editor.load_history(path);
    }

    // Main input loop.
    loop {
        let line = match editor.readline("> ") {
            Ok(line) => line,
            Err(ReadlineError::Eof) => break,
            Err(ReadlineError::Interrupted) => continue,
            Err(e) => return Err(e).context("readline error"),
        };

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        // Add to history (filter out duplicates at the front).
        let _ = editor.add_history_entry(input);

        if input.starts_with('/') {
            match handle_input(input, &mut rho, &mut renderer).await? {
                Action::Continue => {}
                Action::Quit => break,
            }
            continue;
        }

        // Regular text → send as `prompt`.
        renderer.reset_prompt_state();
        let prompt_id = rho.send("prompt", json!({"message": input})).await?;
        process_prompt(&mut rho, &mut renderer, prompt_id).await?;
        println!();
    }

    // Save history and clean up.
    if let Some(ref path) = history_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = editor.save_history(path);
    }

    println!("\n{}", "goodbye.".dimmed());
    rho.kill();
    Ok(())
}
