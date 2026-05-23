//! REPL interaction modes for rho.
//!
//! Two modes:
//! - [`run_repl`] — interactive read-eval-print loop
//! - [`run_prompt_file`] — read a prompt from a file, run once, exit
//!
//! Slash commands:
//! - `/quit` — exit the REPL
//! - `/clear` — branch back to the system message
//! - `/models` — list all models across all providers
//! - `/model <id>` — switch to a model (fuzzy match or `provider/model` syntax)
//! - `/paste` — read multi-line input from stdin (Ctrl-D to finish)
//! - `/paste <file>` — read input from a file as if pasted
//!
//! ## Multi-line input
//!
//! The `/paste` command switches the REPL into multi-line mode. This is
//! useful for pasting code snippets, error messages, or any content that
//! spans multiple lines. In most terminals, you can just type `/paste`
//! then Ctrl-Shift-V (or right-click paste), then press Enter twice
//! (or Ctrl-D) to finish.
//!
//! End-of-input can be triggered three ways:
//! 1. **Ctrl-D** (Unix/macOS) or **Ctrl-Z** (Windows) — sends EOF
//! 2. **Empty line** after at least one line of content
//! 3. **A line containing only `---`** — explicit sentinel
//!

use crate::app::App;
use anyhow::Result;
use rho_core::{AgentObserver, AgentState, ToolResult, ToolRisk};
use std::{
    fs,
    io::{self, BufRead, Write},
};

// ── ReplObserver ──────────────────────────────────────────────────────────────

/// Observer that prints agent events to the REPL.
///
/// Streams reasoning deltas and tool activity to stdout so the user can
/// see what the model is doing while it works. Text deltas from the final
/// response are suppressed here — the complete text is printed once by the
/// REPL after `run_loop` returns.
///
/// Writes to stdout (not stderr) to avoid terminals that render stderr
/// in a different color.
struct ReplObserver;

impl AgentObserver for ReplObserver {
    fn on_state_change(&self, state: AgentState) {
        match state {
            AgentState::Thinking => {
                print!("\n⏳ ");
                let _ = io::stdout().flush();
            }
            AgentState::AwaitingApproval | AgentState::ExecutingTool | AgentState::Idle => {}
        }
    }

    fn on_text_delta(&self, _delta: &str) {
        // Intentionally suppressed — the full text is printed by the REPL
        // after run_loop returns. Streaming partial text would interleave
        // with tool activity output.
    }

    fn on_reasoning_delta(&self, delta: &str) {
        print!("{delta}");
        let _ = io::stdout().flush();
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        // Show a compact one-line summary of the tool call.
        // Truncate arguments to keep it readable.
        let preview = if arguments.len() > 120 {
            format!("{}…", &arguments[..120])
        } else {
            arguments.to_owned()
        };
        println!("\n→ {name}: {preview}");
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        if result.is_error {
            // Show a short error summary.
            let preview = if result.output.len() > 100 {
                format!("{}…", &result.output[..100])
            } else {
                result.output.clone()
            };
            println!("✗ {name}: {preview}");
        }
    }

    fn on_tool_denied(&self, name: &str) {
        println!("⊘ {name}: denied");
    }

    fn on_approval_requested(&self, tool_name: &str, risk: ToolRisk) {
        let risk_label = match risk {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Destructive => "destructive",
        };
        println!("⚠ {tool_name} ({risk_label}) requires approval");
    }
}

// ── REPL ──────────────────────────────────────────────────────────────────────

/// Run the interactive REPL loop.
///
/// Reads lines from stdin, dispatches slash commands, and drives the agent
/// loop for user messages. Handles `/quit`, `/clear`, `/models`, `/model`,
/// `/paste`, and empty-input graceful exit on EOF.
#[allow(clippy::too_many_lines)]
pub async fn run_repl(app: &mut App) -> Result<()> {
    loop {
        print!("User: ");
        io::stdout().flush()?;

        let input = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let bytes_read = std::io::stdin().read_line(&mut line).unwrap_or(0);
            (line, bytes_read)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking failed: {e}"))?;

        // EOF on stdin (bytes_read == 0) — exit gracefully.
        if input.1 == 0 {
            app.session.close("stdin EOF");
            break;
        }

        let input = input.0.trim();

        match input {
            "/quit" | "quit" => {
                app.session.close("user quit");
                break;
            }
            "/clear" => {
                // Branch back to the system message — same effect as
                // clearing the conversation, but the old tree is preserved
                // on disk so it can be inspected or resumed later.
                let path = app.session.path_to_root();
                if let Some(root_entry) = path.last() {
                    let root_id = root_entry.id.clone();
                    let _ = app.session.branch_to(&root_id);
                }
                println!("[conversation cleared]");
                continue;
            }
            "/models" => {
                list_models(app).await;
                continue;
            }
            "/sessions" => {
                list_sessions(app);
                continue;
            }
            _ if input.starts_with("/model ") => {
                switch_model(app, input.strip_prefix("/model ").unwrap().trim()).await;
                continue;
            }
            "/paste" | "/paste " => {
                let file_arg = input
                    .strip_prefix("/paste ")
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let pasted = if let Some(path) = file_arg {
                    // /paste <file> — read content from a file
                    match fs::read_to_string(path) {
                        Ok(content) => content,
                        Err(e) => {
                            eprintln!("Error: cannot read `{path}`: {e}");
                            continue;
                        }
                    }
                } else {
                    // /paste — read multi-line from stdin
                    match read_multiline_input() {
                        Ok(Some(text)) => text,
                        Ok(None) => {
                            // User cancelled (Ctrl-C or immediate EOF)
                            continue;
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            continue;
                        }
                    }
                };

                if pasted.trim().is_empty() {
                    continue;
                }

                let client = app.active_provider().clone_boxed_client();
                match rho_core::run_loop(
                    &mut app.session,
                    pasted.trim(),
                    client.as_ref(),
                    &app.registry,
                    &app.config,
                    app.cancel.clone(),
                    &app.gate,
                    &ReplObserver,
                )
                .await
                {
                    Ok(reply) => println!("\nAssistant: {reply}"),
                    Err(e) => eprintln!("\nError: {e}"),
                }
                continue;
            }
            "" => continue,
            _ => {}
        }

        let client = app.active_provider().clone_boxed_client();
        match rho_core::run_loop(
            &mut app.session,
            input,
            client.as_ref(),
            &app.registry,
            &app.config,
            app.cancel.clone(),
            &app.gate,
            &ReplObserver,
        )
        .await
        {
            Ok(reply) => println!("\nAssistant: {reply}"),
            Err(e) => eprintln!("\nError: {e}"),
        }
    }

    Ok(())
}

/// Read a prompt file, run the agent loop once, print the reply, and exit.
///
/// # Errors
///
/// Returns an error if the file cannot be read or the agent loop encounters
/// a fatal error.
pub async fn run_prompt_file(mut app: App, path: std::path::PathBuf) -> Result<()> {
    let input = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("cannot read prompt file `{}`: {e}", path.display()))?;
    eprintln!("using prompt file: {}", path.display());

    let client = app.active_provider().clone_boxed_client();
    match rho_core::run_loop(
        &mut app.session,
        &input,
        client.as_ref(),
        &app.registry,
        &app.config,
        app.cancel.clone(),
        &app.gate,
        &ReplObserver,
    )
    .await
    {
        Ok(reply) => println!("\nAssistant: {reply}"),
        Err(e) => eprintln!("\nError: {e}"),
    }

    app.session.close("prompt file completed");
    Ok(())
}

/// List all available models across all providers.
async fn list_models(app: &App) {
    let all = app.providers.list_all_models().await;
    if all.is_empty() {
        println!("No models available from any provider.");
        return;
    }

    let mut current_provider = "";
    for (provider_name, model) in &all {
        if *provider_name != current_provider {
            if !current_provider.is_empty() {
                println!();
            }
            println!("  [{provider_name}]");
            current_provider = provider_name;
        }
        println!("    {}", model.id);
    }
    println!();
    println!(
        "  {} model(s) across {} provider(s).",
        all.len(),
        app.providers.len()
    );
}

/// Switch to a different model/provider.
///
/// Supports two syntaxes:
/// - `provider/model` — explicit selection (e.g. `openrouter/gpt-4o`)
/// - `model-id` — fuzzy match across all providers (e.g. `gpt-4o`)
///
/// When the match is ambiguous, shows the candidates and asks the user
/// to disambiguate with the `provider/model` syntax.
async fn switch_model(app: &mut App, query: &str) {
    // Try explicit provider/model syntax first.
    if let Some((provider_name, model_id)) = query.split_once('/') {
        if let Some(provider) = app.providers.get(provider_name) {
            match provider.list_models().await {
                Ok(list) => {
                    if let Some(info) = list.data.iter().find(|m| m.id == model_id) {
                        let idx = app
                            .providers
                            .index_of(provider_name)
                            .expect("provider found via get, so index must exist");
                        app.active_provider_index = idx;
                        app.session.set_model(&info.id);
                        println!("Switched to {} (provider: {provider_name})", info.id);
                        return;
                    }
                    println!("Model \"{model_id}\" not found on provider \"{provider_name}\".");
                    return;
                }
                Err(e) => {
                    println!("Could not query provider \"{provider_name}\": {e}");
                    return;
                }
            }
        }
        println!("Provider \"{provider_name}\" not found.");
        return;
    }

    // Fuzzy match across all providers.
    let all = app.providers.list_all_models().await;
    let matches: Vec<_> = all.iter().filter(|(_, m)| m.id.contains(query)).collect();

    match matches.len() {
        0 => {
            println!("No model matching \"{query}\".");
        }
        1 => {
            let (provider_name, info) = matches[0];
            let idx = app
                .providers
                .index_of(provider_name)
                .expect("provider from list_all_models must exist in registry");
            app.active_provider_index = idx;
            app.session.set_model(&info.id);
            println!("Switched to {} (provider: {provider_name})", info.id);
        }
        _ => {
            println!(
                "Ambiguous match ({} results). Use provider/model syntax:",
                matches.len()
            );
            for (provider_name, info) in &matches {
                println!("  {provider_name}/{}", info.id);
            }
        }
    }
}

/// Maximum number of sessions to display in `/sessions`.
const MAX_SESSIONS_SHOWN: usize = 10;

/// List recent sessions for this project.
///
/// Shows up to [`MAX_SESSIONS_SHOWN`] sessions sorted by modification time
/// (most recent first). Marks the session that `rho -c` would resume.
fn list_sessions(app: &App) {
    let cwd = app.session.header().cwd.clone();
    let sessions = rho_core::list_sessions(&cwd);

    if sessions.is_empty() {
        println!("No previous sessions for this project.");
        return;
    }

    println!("  Recent sessions:");
    let count = sessions.len().min(MAX_SESSIONS_SHOWN);
    for (i, meta) in sessions.iter().take(count).enumerate() {
        let datetime = format_mtime(meta.mtime);
        let size_kb = std::fs::metadata(&meta.path).map_or(0, |m| m.len() / 1024);
        let latest = if i == 0 { "  \u{2190} latest" } else { "" };
        println!(
            "    [{idx}] {datetime}  {size_kb:>5} KB  {entries:>3} entries{latest}",
            idx = i + 1,
            entries = meta.entry_count,
        );
    }

    let remaining = sessions.len().saturating_sub(count);
    if remaining > 0 {
        println!("    ... and {remaining} more");
    }
    println!();
    println!("  Resume with: rho -c");
}

/// Format a filesystem modification time for display.
///
/// Shows the UTC date and time in `YYYY-MM-DD HH:MM` format.
/// Falls back to the epoch if the time cannot be converted.
fn format_mtime(mtime: std::time::SystemTime) -> String {
    let secs = mtime
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_local_time(secs)
}

/// Convert Unix epoch seconds to a `YYYY-MM-DD HH:MM` string.
///
/// UTC-based formatter (no timezone dependency). Good enough for a session
/// listing where exact local time isn't critical.
fn format_local_time(epoch_secs: u64) -> String {
    let days_since_epoch = epoch_secs / 86_400;
    let time_of_day = epoch_secs % 86_400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;

    // Compute year/month/day from days since 1970-01-01.
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    #[allow(clippy::cast_possible_truncation)]
    let (year, month, day) = civil_from_days(days_since_epoch as i32);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}")
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Based on Howard Hinnant's civil calendar algorithm.
#[allow(clippy::cast_sign_loss)]
fn civil_from_days(z: i32) -> (i32, u32, u32) {
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i32::from(m <= 2), m as u32, d as u32)
}

/// Sentinel line that terminates multi-line paste mode.
const PASTE_SENTINEL: &str = "---";

/// Read multi-line input from stdin until the user signals end-of-input.
///
/// Returns `Ok(Some(text))` with the accumulated text, `Ok(None)` if the
/// user cancelled (immediate EOF with no content), or `Err` on I/O failure.
///
/// Three ways to terminate:
/// 1. **Ctrl-D** (Unix/macOS) or **Ctrl-Z** (Windows) — sends EOF
/// 2. **Empty line** after at least one line of content
/// 3. **A line containing only `---`** — explicit sentinel
fn read_multiline_input() -> anyhow::Result<Option<String>> {
    eprintln!("  Entering paste mode. Paste your text, then:");
    eprintln!("    • Press Enter twice (empty line) to finish");
    eprintln!("    • Type --- on its own line to finish");
    eprintln!("    • Press Ctrl-D / Ctrl-Z to finish");
    eprint!("  paste> ");
    io::stderr().flush().ok();

    let stdin = io::stdin();
    let mut lines: Vec<String> = Vec::new();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => return Err(anyhow::anyhow!("read error: {e}")),
        };

        // Explicit sentinel
        if line.trim() == PASTE_SENTINEL && !lines.is_empty() {
            eprintln!("  [paste: {} line(s)]", lines.len());
            break;
        }

        // Empty line after content — terminate
        if line.is_empty() && !lines.is_empty() {
            eprintln!("  [paste: {} line(s)]", lines.len());
            break;
        }

        // Skip leading empty lines
        if line.is_empty() {
            continue;
        }

        lines.push(line);
        eprint!("  ...   ");
        io::stderr().flush().ok();
    }

    if lines.is_empty() {
        // No content read at all (immediate EOF or only whitespace)
        return Ok(None);
    }

    Ok(Some(lines.join("\n")))
}
