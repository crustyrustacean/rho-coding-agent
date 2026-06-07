//! Terminal rendering of JSON-RPC 2.0 notifications and responses.
//!
//! [`Renderer`] tracks streaming state (text and reasoning) so that
//! trailing newlines are inserted at the right places and duplicate
//! text is suppressed when the reply was already streamed.

use colored::Colorize;
use serde_json::Value;
use std::io::Write;

/// Mutable rendering state.
pub(crate) struct Renderer {
    /// True while a `message/delta` stream is active (defer trailing newline).
    in_text: bool,
    /// True while a `reasoning/delta` stream is active.
    in_reasoning: bool,
    /// True if any text was streamed during the current prompt.
    text_was_streamed: bool,
}

impl Renderer {
    /// Create a new renderer with clean state.
    pub(crate) fn new() -> Self {
        Self {
            in_text: false,
            in_reasoning: false,
            text_was_streamed: false,
        }
    }

    /// End any active streaming block (prints a trailing newline if needed).
    pub(crate) fn end_stream(&mut self) {
        if self.in_reasoning || self.in_text {
            let _ = std::io::stdout().flush();
            println!();
            self.in_text = false;
            self.in_reasoning = false;
        }
    }

    /// Reset per-prompt tracking. Call at the start of each user prompt.
    pub(crate) fn reset_prompt_state(&mut self) {
        self.text_was_streamed = false;
    }

    /// Render a JSON-RPC 2.0 notification.
    pub(crate) fn notification(&mut self, method: &str, params: &Value) {
        match method {
            "ready" => {
                // Startup signal — rendered once, caller adds context after.
            }

            "agent/start" => {
                self.end_stream();
            }

            "agent/end" => {
                // Reply content is in the response, not rendered here.
                self.end_stream();
            }

            "agent/error" => {
                self.end_stream();
                if let Some(error) = params.get("error").and_then(|v| v.as_str()) {
                    eprintln!("{} {error}", "error:".red().bold());
                }
            }

            "state/change" => {
                // Too noisy for interactive use; skip.
            }

            "message/delta" => {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    print!("{delta}");
                    let _ = std::io::stdout().flush();
                    self.in_text = true;
                    self.text_was_streamed = true;
                }
            }

            "reasoning/delta" => {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    print!("{}", delta.dimmed());
                    let _ = std::io::stdout().flush();
                    self.in_reasoning = true;
                }
            }

            "tool/call" => {
                self.end_stream();
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  {} {name}", "⏵".cyan());
            }

            "tool/result" => {
                self.end_stream();
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let is_error = params
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let output = params.get("output").and_then(|v| v.as_str()).unwrap_or("");

                if is_error {
                    println!("  {} {name}", "✗".red());
                    if let Some(first) = output.lines().next() {
                        println!("    {}", first.red().dimmed());
                    }
                } else {
                    let total = output.lines().count();
                    for line in output.lines().take(3) {
                        println!("    {}", line.dimmed());
                    }
                    if total > 3 {
                        println!("    {} ({} more lines)", "...".dimmed(), total - 3);
                    }
                }
            }

            "tool/denied" => {
                self.end_stream();
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  {} {name}", "⚠ denied:".yellow());
            }

            // "approval/request" is handled by the caller, not rendered here.
            _ => {}
        }
    }

    /// Render the result of a successful JSON-RPC response.
    pub(crate) fn response(&mut self, method: &str, result: &Value) {
        match method {
            "prompt" => {
                if let Some(reply) = result.get("reply").and_then(|v| v.as_str()) {
                    // If text was streamed via message/delta, the reply was
                    // already printed. Otherwise, print it now.
                    if !self.text_was_streamed {
                        println!("\n{reply}");
                    }
                }
                if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
                    self.end_stream();
                    eprintln!("{} {error}", "error:".red().bold());
                }
            }

            "clear" => {
                println!("{}", "conversation cleared.".dimmed());
            }

            "setModel" => {
                if let Some(model) = result.get("model").and_then(|v| v.as_str()) {
                    if let Some(provider) = result.get("provider").and_then(|v| v.as_str()) {
                        println!("model: {} (provider: {})", model.green(), provider.dimmed());
                    } else {
                        println!("model: {}", model.green());
                    }
                }
            }

            "getState" => {
                let model = result.get("model").and_then(|v| v.as_str()).unwrap_or("?");
                let provider = result
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                println!("model: {model} (provider: {provider})");
            }

            "listModels" => {
                if let Some(models) = result.get("models").and_then(|v| v.as_array()) {
                    if models.is_empty() {
                        println!("no models available");
                    } else {
                        println!("{}", "available models:".bold());
                        for m in models {
                            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            let provider =
                                m.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                            println!("  {id} ({provider})");
                        }
                    }
                }
            }

            "listProviders" => {
                if let Some(providers) = result.get("providers").and_then(|v| v.as_array()) {
                    if providers.is_empty() {
                        println!("no providers configured");
                    } else {
                        println!("{}", "configured providers:".bold());
                        for p in providers {
                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let is_active =
                                p.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                            let reachable = p
                                .get("reachable")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let is_external = p
                                .get("isExternal")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            let status = if is_active {
                                "*".green().to_string()
                            } else {
                                " ".to_string()
                            };
                            let kind = if is_external { "remote" } else { "local" };
                            let health = if reachable {
                                "ok".green().to_string()
                            } else {
                                "down".red().to_string()
                            };
                            println!("  {status} {name} ({kind}, {health})");
                        }
                        println!("  {} = active", "*".green());
                    }
                }
            }

            "getSessionStats" => {
                let cw = result
                    .get("contextWindow")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let used = result
                    .get("estimatedUsed")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let pct = result
                    .get("utilizationPercent")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let msgs = result
                    .get("messageCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!("context: {used}/{cw} tokens ({pct:.1}%), {msgs} messages");
            }

            "listSessions" => {
                if let Some(sessions) = result.get("sessions").and_then(|v| v.as_array()) {
                    if sessions.is_empty() {
                        println!("no previous sessions");
                    } else {
                        println!("{}", "previous sessions:".bold());
                        for s in sessions {
                            let path = s.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                            let entries = s.get("entryCount").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!("  {path} ({entries} entries)");
                        }
                    }
                }
            }

            "listExtensions" => {
                if let Some(exts) = result.get("extensions").and_then(|v| v.as_array()) {
                    if exts.is_empty() {
                        println!("no extensions loaded");
                    } else {
                        println!("{}", "extensions:".bold());
                        for ext in exts {
                            let name = ext.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let tools = ext
                                .get("tools")
                                .and_then(|v| v.as_array())
                                .map_or(0, Vec::len);
                            println!("  {name} ({tools} tools)");
                        }
                    }
                }
            }

            "reloadExtensions" => {
                let added = result.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
                let reloaded = result.get("reloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                let removed = result.get("removed").and_then(|v| v.as_u64()).unwrap_or(0);
                let failed = result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("extensions reloaded: +{added} ~{reloaded} -{removed} ✗{failed}");
            }

            "compact" => {
                println!("{}", "context compacted.".green());
            }

            "abort" => {}

            _ => {
                let _ = writeln!(std::io::stdout(), "{result}");
            }
        }
    }

    /// Render a JSON-RPC 2.0 error response.
    pub(crate) fn error_response(&mut self, code: i32, message: &str) {
        eprintln!("{} [{code}] {message}", "error:".red().bold());
    }
}
