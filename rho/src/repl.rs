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

use crate::app::App;
use anyhow::Result;
use rho_core::{AgentObserver, AgentState, ToolResult, ToolRisk};
use std::{
    fs,
    io::{self, Write},
};

// ── ReplObserver ──────────────────────────────────────────────────────────────

/// Observer that prints agent events to the REPL.
///
/// Streams reasoning deltas and tool activity to stderr so the user can
/// see what the model is doing while it works. Text deltas from the final
/// response are suppressed here — the complete text is printed once by the
/// REPL after `run_loop` returns.
struct ReplObserver;

impl AgentObserver for ReplObserver {
    fn on_state_change(&self, state: AgentState) {
        match state {
            AgentState::Thinking => eprint!("\n⏳ "),
            AgentState::AwaitingApproval | AgentState::ExecutingTool | AgentState::Idle => {}
        }
    }

    fn on_text_delta(&self, _delta: &str) {
        // Intentionally suppressed — the full text is printed by the REPL
        // after run_loop returns. Streaming partial text would interleave
        // with tool activity output.
    }

    fn on_reasoning_delta(&self, delta: &str) {
        eprint!("{delta}");
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        // Show a compact one-line summary of the tool call.
        // Truncate arguments to keep it readable.
        let preview = if arguments.len() > 120 {
            format!("{}…", &arguments[..120])
        } else {
            arguments.to_owned()
        };
        eprintln!("\n→ {name}: {preview}");
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        if result.is_error {
            // Show a short error summary.
            let preview = if result.output.len() > 100 {
                format!("{}…", &result.output[..100])
            } else {
                result.output.clone()
            };
            eprintln!("✗ {name}: {preview}");
        }
    }

    fn on_tool_denied(&self, name: &str) {
        eprintln!("⊘ {name}: denied");
    }

    fn on_approval_requested(&self, tool_name: &str, risk: ToolRisk) {
        let risk_label = match risk {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Destructive => "destructive",
        };
        eprintln!("⚠ {tool_name} ({risk_label}) requires approval");
    }
}

// ── REPL ──────────────────────────────────────────────────────────────────────

/// Run the interactive REPL loop.
///
/// Reads lines from stdin, dispatches slash commands, and drives the agent
/// loop for user messages. Handles `/quit`, `/clear`, `/models`, `/model`,
/// and empty-input graceful exit on EOF.
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
            _ if input.starts_with("/model ") => {
                switch_model(app, input.strip_prefix("/model ").unwrap().trim()).await;
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
