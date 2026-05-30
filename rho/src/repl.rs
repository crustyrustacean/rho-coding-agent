//! REPL interaction mode for rho.
//!
//! [`run_repl`] — interactive read-eval-print loop
//!
//! Slash commands:
//! - `/quit` — exit the REPL
//! - `/clear` — branch back to the system message
//! - `/models` — list all models across all providers
//! - `/model <id>` — switch to a model (fuzzy match or `provider/model` syntax)
//! - `/paste` — read multi-line input from stdin (Ctrl-D to finish)
//! - `/paste <file>` — read input from a file as if pasted
//! - `/reload` — hot-reload TypeScript extensions
//! - `/extensions` — list loaded extensions
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
//! All formatted output is delegated to [`crate::presenter::ReplPresenter`].

use crate::app::App;
use crate::ext_observer::CompositeObserver;
use crate::gate::ReplApprovalGate;
use crate::presenter::ReplPresenter as P;
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
/// This is the *agent-event* subset of the presenter. It implements a
/// `rho-core` trait, so it lives here rather than in `presenter.rs`.
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
        // after run_loop returns.
    }

    fn on_reasoning_delta(&self, delta: &str) {
        print!("{delta}");
        let _ = io::stdout().flush();
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        let preview = if arguments.len() > 120 {
            format!("{}…", &arguments[..120])
        } else {
            arguments.to_owned()
        };
        println!("\n→ {name}: {preview}");
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        if result.is_error {
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
            ToolRisk::Network => "network",
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
    let gate = ReplApprovalGate;
    let repl_observer = ReplObserver;
    loop {
        P::user_prompt();

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
                let path = app.session.path_to_root();
                if let Some(root_entry) = path.last() {
                    let root_id = root_entry.id.clone();
                    let _ = app.session.branch_to(&root_id);
                }
                P::conversation_cleared();
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
            "/status" | "/context" => {
                show_context_stats(app);
                continue;
            }
            "/reload" => {
                reload_extensions(app).await;
                continue;
            }
            "/extensions" => {
                list_extensions(app);
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
                    match fs::read_to_string(path) {
                        Ok(content) => content,
                        Err(e) => {
                            P::paste_read_error(path, &e.to_string());
                            continue;
                        }
                    }
                } else {
                    match read_multiline_input() {
                        Ok(Some(text)) => text,
                        Ok(None) => continue,
                        Err(e) => {
                            P::error(&e.to_string());
                            continue;
                        }
                    }
                };

                if pasted.trim().is_empty() {
                    continue;
                }

                let client = app.active_provider().clone_boxed_service();
                let composite = build_composite(&repl_observer, &app.ext_observers);
                let params = rho_core::LoopParams {
                    client: client.as_ref(),
                    registry: &app.registry,
                    config: &app.config,
                    cancel: app.cancel.clone(),
                    gate: &gate,
                    observer: &composite,
                };
                match rho_core::run_loop(&mut app.session, pasted.trim(), &params).await {
                    Ok(reply) => P::assistant_reply(&reply),
                    Err(e) => P::error(&e.to_string()),
                }
                print_context_bar(&app.session);
                continue;
            }
            "" => continue,
            _ => {}
        }

        let client = app.active_provider().clone_boxed_service();
        let composite = build_composite(&repl_observer, &app.ext_observers);
        let params = rho_core::LoopParams {
            client: client.as_ref(),
            registry: &app.registry,
            config: &app.config,
            cancel: app.cancel.clone(),
            gate: &gate,
            observer: &composite,
        };
        match rho_core::run_loop(&mut app.session, input, &params).await {
            Ok(reply) => P::assistant_reply(&reply),
            Err(e) => P::error(&e.to_string()),
        }
        print_context_bar(&app.session);
    }

    Ok(())
}

/// List all available models across all providers.
async fn list_models(app: &App) {
    let all = app.providers.list_all_models().await;
    if all.is_empty() {
        P::no_models();
        return;
    }

    let mut current_provider = "";
    for (provider_name, model) in &all {
        if *provider_name != current_provider {
            println!();
            P::models_provider_header(provider_name);
            current_provider = provider_name;
        }
        P::models_entry(&model.id);
    }
    P::models_summary(all.len(), app.providers.len());
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
                        app.ext_loader.set_model_all(&info.id).await;
                        P::model_switched(&info.id, provider_name);
                        return;
                    }
                    P::model_not_found_on_provider(model_id, provider_name);
                    return;
                }
                Err(e) => {
                    P::provider_query_error(provider_name, &e.to_string());
                    return;
                }
            }
        }
        P::provider_not_found(provider_name);
        return;
    }

    let all = app.providers.list_all_models().await;
    let matches: Vec<_> = all.iter().filter(|(_, m)| m.id.contains(query)).collect();

    match matches.len() {
        0 => {
            P::no_model_match(query);
        }
        1 => {
            let (provider_name, info) = matches[0];
            let idx = app
                .providers
                .index_of(provider_name)
                .expect("provider from list_all_models must exist in registry");
            app.active_provider_index = idx;
            app.session.set_model(&info.id);
            app.ext_loader.set_model_all(&info.id).await;
            P::model_switched(&info.id, provider_name);
        }
        _ => {
            let match_pairs: Vec<(String, String)> = matches
                .iter()
                .map(|(p, m)| ((*p).to_owned(), m.id.clone()))
                .collect();
            P::ambiguous_match(&match_pairs);
        }
    }
}

/// Maximum number of sessions to display in `/sessions`.
const MAX_SESSIONS_SHOWN: usize = 10;

/// List recent sessions for this project.
fn list_sessions(app: &App) {
    let cwd = app.session.header().cwd.clone();
    let sessions = rho_core::list_sessions(&cwd);

    if sessions.is_empty() {
        P::no_sessions();
        return;
    }

    P::sessions_header();
    let count = sessions.len().min(MAX_SESSIONS_SHOWN);
    for (i, meta) in sessions.iter().take(count).enumerate() {
        let datetime = format_mtime(meta.mtime);
        let size_kb = std::fs::metadata(&meta.path).map_or(0, |m| m.len() / 1024);
        P::sessions_entry(i + 1, &datetime, size_kb, meta.entry_count, i == 0);
    }

    let remaining = sessions.len().saturating_sub(count);
    if remaining > 0 {
        P::sessions_truncated(remaining);
    }
    P::sessions_resume_hint();
}

/// Print a compact one-line context usage bar after each turn.
fn print_context_bar(session: &rho_core::Session) {
    let stats = session.context_stats();
    let pct = stats.utilization_percent();
    #[allow(clippy::cast_precision_loss)]
    let used_k = stats.estimated_used as f64 / 1000.0;
    #[allow(clippy::cast_precision_loss)]
    let window_k = stats.context_window as f64 / 1000.0;
    #[allow(clippy::cast_precision_loss)]
    let remaining_k = stats.estimated_remaining() as f64 / 1000.0;

    P::context_bar(pct, used_k, window_k, remaining_k, stats.message_count);
}

/// Show detailed context status (the `/status` command).
fn show_context_stats(app: &App) {
    let stats = app.session.context_stats();
    let prompt_budget = stats
        .context_window
        .saturating_sub(stats.completion_reserve);
    let system_overhead = app.session.system_overhead();
    let schema_overhead = app.session.schema_overhead();
    let conv_tokens = stats
        .estimated_used
        .saturating_sub(system_overhead)
        .saturating_sub(schema_overhead);
    let msg_budget = app.session.message_budget();

    P::context_status(
        stats.context_window,
        stats.completion_reserve,
        prompt_budget,
        system_overhead,
        schema_overhead,
        conv_tokens,
        stats.estimated_used,
        stats.estimated_remaining(),
        stats.utilization_percent(),
        stats.message_count,
        stats.path_entry_count,
        stats.entry_count,
        msg_budget,
        app.session.model(),
        app.session.save_path(),
    );
}

/// Build a composite observer from the REPL observer and extension observers.
fn build_composite<'a>(
    repl: &'a ReplObserver,
    ext_observers: &'a [rho_ext::DenoObserver],
) -> CompositeObserver<'a> {
    let mut observers: Vec<&'a dyn AgentObserver> = vec![repl];
    for ext_obs in ext_observers {
        observers.push(ext_obs);
    }
    CompositeObserver::new(observers)
}

/// Hot-reload extensions.
///
/// Re-reads the extension config from disk before reloading so that
/// newly-created or newly-enabled extensions are picked up.
async fn reload_extensions(app: &mut App) {
    let dirs = crate::app::extension_dirs(&app.session.header().cwd);

    // Re-read the config so that extensions added to the enabled list
    // during this session are picked up by the filter.
    let sandbox_path = app.session.header().cwd.clone();
    let fresh_config = rho_core::ConfigLoader::load(&sandbox_path).unwrap_or_else(|e| {
        P::error(&format!("config reload failed: {e}"));
        rho_core::RhoConfig::default()
    });
    app.ext_loader.set_config(fresh_config.extensions);

    match app.ext_loader.reload(&dirs, &mut app.registry).await {
        Ok(report) => {
            // Refresh the extension observers after reload.
            app.ext_observers = app.ext_loader.build_observers();
            P::extension_reload_report(
                report.added.len(),
                report.reloaded.len(),
                report.removed.len(),
                report.failed.len(),
            );
        }
        Err(e) => P::error(&format!("extension reload failed: {e}")),
    }
}

/// List loaded extensions.
fn list_extensions(app: &App) {
    let names = app.ext_loader.loaded_names();
    P::extension_list(&names);
}

/// Format a filesystem modification time for display.
fn format_mtime(mtime: std::time::SystemTime) -> String {
    let secs = mtime
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_local_time(secs)
}

/// Convert Unix epoch seconds to a `YYYY-MM-DD HH:MM` string.
fn format_local_time(epoch_secs: u64) -> String {
    let days_since_epoch = epoch_secs / 86_400;
    let time_of_day = epoch_secs % 86_400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;

    #[allow(clippy::cast_possible_truncation)]
    let (year, month, day) = civil_from_days(days_since_epoch as i32);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}")
}

/// Convert days since Unix epoch to (year, month, day).
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
fn read_multiline_input() -> anyhow::Result<Option<String>> {
    P::paste_header();
    P::paste_prompt();

    let stdin = io::stdin();
    let mut lines: Vec<String> = Vec::new();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => return Err(anyhow::anyhow!("read error: {e}")),
        };

        if line.trim() == PASTE_SENTINEL && !lines.is_empty() {
            P::paste_complete(lines.len());
            break;
        }

        if line.is_empty() && !lines.is_empty() {
            P::paste_complete(lines.len());
            break;
        }

        if line.is_empty() {
            continue;
        }

        lines.push(line);
        P::paste_continuation();
    }

    if lines.is_empty() {
        return Ok(None);
    }

    Ok(Some(lines.join("\n")))
}
