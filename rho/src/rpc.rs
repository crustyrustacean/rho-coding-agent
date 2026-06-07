//! Headless JSON-RPC 2.0 mode — JSONL over stdin/stdout.
//!
//! In RPC mode rho reads newline-delimited JSON-RPC 2.0 requests from stdin and
//! writes JSON-RPC 2.0 responses to stdout. Streaming events are delivered as
//! JSON-RPC notifications (no `id` field).
//!
//! # Protocol
//!
//! Every request must be a JSON object with `jsonrpc: "2.0"`, a `method` field,
//! optional `params`, and a numeric or string `id` for response correlation.
//!
//! ## Methods (stdin → rho)
//!
//! | Method             | Params                      | Description                        |
//! |--------------------|-----------------------------|------------------------------------|
//! | `prompt`           | `{message: string}`         | Send a user message to the agent   |
//! | `abort`            | —                           | Cancel the current operation       |
//! | `clear`            | —                           | Clear conversation history         |
//! | `getState`         | —                           | Return model, provider, and cwd  |
//! | `getMessages`      | —                           | Return all messages on active path |
//! | `setModel`         | `{model: string}`          | Switch the active model            |
//! | `listModels`       | —                           | List available models from providers |
//! | `getSessionStats`  | —                           | Return token budget / usage info   |
//! | `listSessions`     | —                           | List previous sessions for project |
//! | `listExtensions`   | —                           | List loaded extensions and tools   |
//! | `reloadExtensions` | —                           | Reload extensions from disk        |
//! | `compact`          | —                           | Trigger context compaction         |
//!
//! ## Notifications (rho → stdout)
//!
//! | Method              | Params                              | Description                         |
//! |---------------------|-------------------------------------|-------------------------------------|
//! | `ready`             | —                                   | Emitted once on startup             |
//! | `agent/start`       | —                                   | Agent began processing a prompt     |
//! | `agent/end`         | `{reply: string}`                   | Agent finished; full text reply     |
//! | `agent/error`       | `{error: string}`                   | Agent loop encountered an error     |
//! | `state/change`      | `{state: string}`                   | Loop state transition               |
//! | `message/delta`     | `{delta: string}`                   | Streaming text chunk                |
//! | `reasoning/delta`   | `{delta: string}`                   | Streaming reasoning chunk           |
//! | `tool/call`         | `{name, arguments}`                 | Model requested a tool call         |
//! | `tool/result`       | `{name, is_error, output}`          | Tool finished executing             |
//! | `tool/denied`       | `{name}`                            | Tool call denied by approval gate   |
//! | `approval/request`  | `{tool, arguments, risk}`           | Approval required; send response    |
//!
//! ## Approval flow
//!
//! When rho emits an `approval/request` notification it blocks until it reads
//! an `approvalResponse` method from stdin:
//!
//! ```json
//! {"jsonrpc": "2.0", "method": "approvalResponse", "params": {"approved": true}, "id": 2}
//! ```
//!
//! Sending `approved: false` denies the tool call and lets the agent continue.
//!
//! ## Error codes
//!
//! Standard JSON-RPC 2.0 error codes are used:
//!
//! | Code   | Meaning              |
//! |--------|----------------------|
//! | -32700 | Parse error          |
//! | -32600 | Invalid request      |
//! | -32601 | Method not found     |
//! | -32602 | Invalid params       |
//! | -32603 | Internal error       |

use crate::app::{App, TurnResult, run_agent_turn};
use crate::ext_observer::CompositeObserver;
use anyhow::Result;
use async_trait::async_trait;
use rho_core::{
    AgentObserver, AgentState, ApprovalGate, ChatMessage, ContentBlock, ModelToolCall, ToolResult,
    ToolRisk,
};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

// ── JSON-RPC 2.0 Error codes ───────────────────────────────────────────────────

/// Parse error: Invalid JSON was received.
const PARSE_ERROR: i32 = -32700;
/// Invalid request: The JSON sent is not a valid Request object.
const INVALID_REQUEST: i32 = -32600;
/// Method not found: The method does not exist / is not available.
const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid params: Invalid method parameter(s).
const INVALID_PARAMS: i32 = -32602;
/// Internal error: Internal JSON-RPC error.
const INTERNAL_ERROR: i32 = -32603;

// ── Shared I/O types ──────────────────────────────────────────────────────────

/// Shared, mutex-protected writer used by the command loop, observer,
/// and approval gate to write JSONL events without interleaving.
type Out = Arc<Mutex<Box<dyn Write + Send + Sync>>>;

/// Shared, mutex-protected reader for stdin, shared between the command loop
/// and the approval gate (which reads approval responses during `run_loop`).
type In = Arc<Mutex<Box<dyn BufRead + Send>>>;

/// Wrap any [`Write`] + [`Send`] + [`Sync`] sink as an [`Out`].
fn make_out<W: Write + Send + Sync + 'static>(w: W) -> Out {
    Arc::new(Mutex::new(Box::new(w)))
}

/// Wrap any [`BufRead`] + [`Send`] reader as an [`In`].
fn make_in<R: BufRead + Send + 'static>(r: R) -> In {
    Arc::new(Mutex::new(Box::new(r)))
}

/// Write a single JSON-RPC message to `out`.
fn write_jsonrpc(out: &Out, value: &Value) {
    let mut guard = out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_value_to(&mut *guard, value);
}

/// Write a JSON value to any [`Write`] sink.
fn write_value_to(sink: &mut dyn Write, value: &Value) {
    let _ = writeln!(sink, "{value}");
    let _ = sink.flush();
}

// ── JSON-RPC response builders ────────────────────────────────────────────────

/// Build a successful JSON-RPC response.
#[allow(clippy::needless_pass_by_value)]
fn success_response(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id
    })
}

/// Build a JSON-RPC error response.
fn error_response(id: &Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message
        },
        "id": id
    })
}

/// Build a JSON-RPC notification (no id).
#[allow(clippy::needless_pass_by_value)]
fn notification(method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    })
}

// ── RpcObserver ───────────────────────────────────────────────────────────────

/// Forwards agent-loop events to the RPC client as JSON-RPC notifications.
struct RpcObserver {
    /// Shared writer handle.
    out: Out,
}

impl AgentObserver for RpcObserver {
    fn on_state_change(&self, state: AgentState) {
        write_jsonrpc(
            &self.out,
            &notification("state/change", json!({"state": state_name(&state)})),
        );
    }

    fn on_text_delta(&self, delta: &str) {
        write_jsonrpc(
            &self.out,
            &notification("message/delta", json!({"delta": delta})),
        );
    }

    fn on_reasoning_delta(&self, delta: &str) {
        write_jsonrpc(
            &self.out,
            &notification("reasoning/delta", json!({"delta": delta})),
        );
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        write_jsonrpc(
            &self.out,
            &notification("tool/call", json!({"name": name, "arguments": arguments})),
        );
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        write_jsonrpc(
            &self.out,
            &notification(
                "tool/result",
                json!({
                    "name": name,
                    "is_error": result.is_error,
                    "output": result.output,
                }),
            ),
        );
    }

    fn on_tool_denied(&self, name: &str) {
        write_jsonrpc(
            &self.out,
            &notification("tool/denied", json!({"name": name})),
        );
    }
}

// ── RpcApprovalGate ───────────────────────────────────────────────────────────

/// Writes an `approval/request` notification and reads an `approvalResponse`
/// request from the shared reader.
struct RpcApprovalGate {
    /// Shared writer handle.
    out: Out,
    /// Shared reader handle.
    input: In,
}

#[async_trait]
impl ApprovalGate for RpcApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool {
        write_jsonrpc(
            &self.out,
            &notification(
                "approval/request",
                json!({
                    "tool": &*call.function.name,
                    "arguments": call.function.arguments,
                    "risk": risk_label(risk),
                }),
            ),
        );

        // Read the approvalResponse request from the shared reader.
        let input = Arc::clone(&self.input);
        let response = tokio::task::spawn_blocking(move || {
            let mut guard = input
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut line = String::new();
            guard.read_line(&mut line).ok()?;
            serde_json::from_str::<Value>(line.trim()).ok()
        })
        .await;

        match response {
            Ok(Some(v)) => v["params"]["approved"].as_bool().unwrap_or(false),
            _ => false,
        }
    }
}

// ── run_rpc / run_rpc_on ──────────────────────────────────────────────────────

/// Run the agent in headless JSON-RPC mode with real stdin/stdout.
///
/// # Errors
///
/// Returns an error if the stdin background task fails unexpectedly.
pub async fn run_rpc(app: App) -> Result<()> {
    run_rpc_on(app, io::BufReader::new(io::stdin()), io::stdout()).await
}

/// Core JSON-RPC loop — generic over I/O for testability.
///
/// Emits a `ready` notification, then reads JSON-RPC requests from `input`
/// one line at a time. Each request is dispatched to the appropriate handler.
/// Exits cleanly on input EOF.
///
/// # Errors
///
/// Returns an error if the input background task fails unexpectedly.
pub(crate) async fn run_rpc_on<R, W>(mut app: App, input: R, output: W) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + Sync + 'static,
{
    let out = make_out(output);
    let inp = make_in(input);

    write_jsonrpc(&out, &notification("ready", json!({})));

    loop {
        let inp_clone = Arc::clone(&inp);
        let (buf, bytes_read) = tokio::task::spawn_blocking(move || {
            let mut guard = inp_clone
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut buf = String::new();
            let n = guard.read_line(&mut buf).unwrap_or(0);
            (buf, n)
        })
        .await
        .map_err(|e| anyhow::anyhow!("stdin task failed: {e}"))?;

        // EOF — shut down cleanly.
        if bytes_read == 0 {
            app.session.close("stdin EOF");
            break;
        }

        let raw = buf.trim().to_owned();
        if raw.is_empty() {
            continue;
        }

        // Parse and validate JSON-RPC request.
        let request: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                write_jsonrpc(
                    &out,
                    &error_response(&Value::Null, PARSE_ERROR, &format!("Parse error: {e}")),
                );
                continue;
            }
        };

        // Extract required fields.
        let jsonrpc = request.get("jsonrpc").and_then(|v| v.as_str());
        let method = request.get("method").and_then(|v| v.as_str());
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Validate jsonrpc version.
        if jsonrpc != Some("2.0") {
            write_jsonrpc(
                &out,
                &error_response(&id, INVALID_REQUEST, "Invalid JSON-RPC version"),
            );
            continue;
        }

        // Validate method.
        let Some(method) = method else {
            write_jsonrpc(
                &out,
                &error_response(&id, INVALID_REQUEST, "Missing method field"),
            );
            continue;
        };

        // Dispatch.
        dispatch_request(&mut app, method, params, &id, &out, &inp).await;
    }

    Ok(())
}

// ── Request dispatch ─────────────────────────────────────────────────────────

/// Dispatch a validated JSON-RPC request to its handler.
async fn dispatch_request(
    app: &mut App,
    method: &str,
    params: Value,
    id: &Value,
    out: &Out,
    inp: &In,
) {
    match method {
        "prompt" => handle_prompt(app, params, id, out, inp).await,
        "abort" => {
            app.cancel.cancel();
            write_jsonrpc(out, &success_response(id, json!({})));
        }
        "clear" => handle_clear(app, id, out),
        "getState" => handle_get_state(app, id, out),
        "getMessages" => handle_get_messages(app, id, out),
        "setModel" => handle_set_model(app, params, id, out).await,
        "listModels" => handle_list_models(app, id, out).await,
        "getSessionStats" => handle_get_session_stats(app, id, out),
        "listSessions" => handle_list_sessions(app, id, out),
        "listExtensions" => handle_list_extensions(app, id, out),
        "reloadExtensions" => handle_reload_extensions(app, id, out).await,
        "compact" => handle_compact(app, id, out).await,
        "approvalResponse" => {
            // Handled synchronously during approval flow, but if we see it here,
            // acknowledge it (shouldn't normally happen outside approval flow).
            write_jsonrpc(out, &success_response(id, json!({})));
        }
        _ => write_jsonrpc(
            out,
            &error_response(id, METHOD_NOT_FOUND, &format!("Method not found: {method}")),
        ),
    }
}

// ── Request handlers ─────────────────────────────────────────────────────────

/// Run one agent turn for the user message.
async fn handle_prompt(app: &mut App, params: Value, id: &Value, out: &Out, inp: &In) {
    let message = match params.get("message").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => m.to_owned(),
        _ => {
            write_jsonrpc(
                out,
                &error_response(
                    id,
                    INVALID_PARAMS,
                    "prompt requires a non-empty 'message' param",
                ),
            );
            return;
        }
    };

    write_jsonrpc(out, &notification("agent/start", json!({})));

    let observer = RpcObserver {
        out: Arc::clone(out),
    };
    let composite = CompositeObserver::new({
        let mut obs: Vec<&dyn AgentObserver> = vec![&observer];
        for ext_obs in &app.ext_observers {
            obs.push(ext_obs);
        }
        obs
    });
    let gate = RpcApprovalGate {
        out: Arc::clone(out),
        input: Arc::clone(inp),
    };
    let client = app.active_provider().clone_boxed_service();
    let compaction_client = if app.config.compaction_mode == "llm" {
        Some(std::sync::Arc::from(
            app.active_provider().clone_boxed_service(),
        ))
    } else {
        None
    };
    let params = rho_core::LoopParams {
        client: client.as_ref(),
        registry: &app.registry,
        config: &app.config,
        cancel: app.cancel.clone(),
        gate: &gate,
        observer: &composite,
        compaction_client,
    };
    match run_agent_turn(&mut app.session, &message, &params).await {
        TurnResult::Reply(reply) => {
            write_jsonrpc(out, &notification("agent/end", json!({"reply": &reply})));
            write_jsonrpc(out, &success_response(id, json!({"reply": reply})));
        }
        TurnResult::Error(e) => {
            write_jsonrpc(out, &notification("agent/error", json!({"error": &e})));
            write_jsonrpc(out, &success_response(id, json!({"error": e})));
        }
    }
}

/// Return the current model, provider, and working directory.
fn handle_get_state(app: &App, id: &Value, out: &Out) {
    write_jsonrpc(
        out,
        &success_response(
            id,
            json!({
                "model": app.session.model(),
                "provider": app.active_provider().name(),
                "cwd": app.session.header().cwd.to_string_lossy(),
            }),
        ),
    );
}

/// Return all messages on the current session path.
fn handle_get_messages(app: &App, id: &Value, out: &Out) {
    let messages: Vec<Value> = app
        .session
        .path_messages()
        .iter()
        .map(message_to_json)
        .collect();
    write_jsonrpc(out, &success_response(id, json!({"messages": messages})));
}

/// Switch the active model.
async fn handle_set_model(app: &mut App, params: Value, id: &Value, out: &Out) {
    match params.get("model").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => {
            app.set_model(m).await;
            write_jsonrpc(out, &success_response(id, json!({"model": m})));
        }
        _ => write_jsonrpc(
            out,
            &error_response(
                id,
                INVALID_PARAMS,
                "setModel requires a non-empty 'model' param",
            ),
        ),
    }
}

/// Return token budget and context-window usage statistics.
fn handle_get_session_stats(app: &App, id: &Value, out: &Out) {
    let stats = app.session.context_stats();
    write_jsonrpc(
        out,
        &success_response(
            id,
            json!({
                "contextWindow": stats.context_window,
                "completionReserve": stats.completion_reserve,
                "estimatedUsed": stats.estimated_used,
                "estimatedRemaining": stats.estimated_remaining(),
                "utilizationPercent": stats.utilization_percent(),
                "messageCount": stats.message_count,
                "entryCount": stats.entry_count,
                "pathEntryCount": stats.path_entry_count,
                "compactedEntryCount": stats.compacted_entry_count,
                "compactionTokens": stats.compaction_tokens,
                "roleTokens": {
                    "system": stats.role_tokens.system,
                    "user": stats.role_tokens.user,
                    "assistant": stats.role_tokens.assistant,
                    "tool": stats.role_tokens.tool,
                },
                "resolutionTokens": {
                    "full": stats.resolution_tokens.full,
                    "outlined": stats.resolution_tokens.outlined,
                    "summarized": stats.resolution_tokens.summarized,
                    "pinned": stats.resolution_tokens.pinned,
                },
            }),
        ),
    );
}

/// Trigger context compaction.
async fn handle_compact(app: &mut App, id: &Value, out: &Out) {
    match app.compact().await {
        Ok(()) => write_jsonrpc(out, &success_response(id, json!({}))),
        Err(e) => write_jsonrpc(out, &error_response(id, INTERNAL_ERROR, &e.to_string())),
    }
}

/// Clear conversation history by branching back to system message.
fn handle_clear(app: &mut App, id: &Value, out: &Out) {
    let path = app.session.path_to_root();
    if let Some(root_entry) = path.last() {
        let root_id = root_entry.id.clone();
        let _ = app.session.branch_to(&root_id);
        write_jsonrpc(out, &success_response(id, json!({})));
    } else {
        write_jsonrpc(
            out,
            &error_response(id, INTERNAL_ERROR, "No root entry to clear to"),
        );
    }
}

/// List available models from all providers.
async fn handle_list_models(app: &App, id: &Value, out: &Out) {
    let all = app.providers.list_all_models().await;
    let models: Vec<Value> = all
        .iter()
        .map(|(provider, info)| {
            json!({
                "id": info.id,
                "provider": provider,
            })
        })
        .collect();
    write_jsonrpc(out, &success_response(id, json!({"models": models})));
}

/// List previous sessions for this project.
fn handle_list_sessions(app: &App, id: &Value, out: &Out) {
    let cwd = app.session.header().cwd.clone();
    let sessions = rho_core::list_sessions(&cwd);

    let list: Vec<Value> = sessions
        .iter()
        .map(|meta| {
            let mtime = meta
                .mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let size_kb = std::fs::metadata(&meta.path).map_or(0, |m| m.len() / 1024);
            json!({
                "path": meta.path,
                "mtimeSecs": mtime,
                "sizeKb": size_kb,
                "entryCount": meta.entry_count,
            })
        })
        .collect();

    write_jsonrpc(out, &success_response(id, json!({"sessions": list})));
}

/// List loaded extensions and their tools.
fn handle_list_extensions(app: &App, id: &Value, out: &Out) {
    let extensions: Vec<Value> = app
        .ext_loader
        .extension_tools()
        .into_iter()
        .map(|(name, tools)| {
            json!({
                "name": name,
                "tools": tools,
            })
        })
        .collect();

    write_jsonrpc(
        out,
        &success_response(id, json!({"extensions": extensions})),
    );
}

/// Reload extensions from disk.
async fn handle_reload_extensions(app: &mut App, id: &Value, out: &Out) {
    let dirs = crate::app::extension_dirs(&app.session.header().cwd);

    let fresh_config = rho_core::ConfigLoader::load(&app.session.header().cwd).unwrap_or_default();
    app.ext_loader.set_config(fresh_config.extensions);

    match app.ext_loader.reload(&dirs, &mut app.registry).await {
        Ok(report) => {
            app.ext_observers = app.ext_loader.build_observers();
            write_jsonrpc(
                out,
                &success_response(
                    id,
                    json!({
                        "added": report.added.len(),
                        "reloaded": report.reloaded.len(),
                        "removed": report.removed.len(),
                        "failed": report.failed.len(),
                    }),
                ),
            );
        }
        Err(e) => write_jsonrpc(out, &error_response(id, INTERNAL_ERROR, &e.to_string())),
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Map an [`AgentState`] to its JSON string label.
fn state_name(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Thinking => "thinking",
        AgentState::AwaitingApproval => "awaiting_approval",
        AgentState::ExecutingTool => "executing_tool",
    }
}

/// Map a [`ToolRisk`] to its JSON string label.
fn risk_label(risk: ToolRisk) -> &'static str {
    match risk {
        ToolRisk::Read => "read",
        ToolRisk::Write => "write",
        ToolRisk::Destructive => "destructive",
        ToolRisk::Network => "network",
    }
}

/// Serialize a [`ChatMessage`] to a JSON value.
fn message_to_json(msg: &ChatMessage) -> Value {
    match msg {
        ChatMessage::System { content } => {
            json!({"role": "system", "content": blocks_to_text(content)})
        }
        ChatMessage::User { content } => {
            json!({"role": "user", "content": blocks_to_text(content)})
        }
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    json!({
                        "id": &*tc.id,
                        "name": &*tc.function.name,
                        "arguments": tc.function.arguments,
                    })
                })
                .collect();
            json!({
                "role": "assistant",
                "content": blocks_to_text(content),
                "toolCalls": calls,
            })
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            let id: &str = tool_call_id;
            json!({
                "role": "tool",
                "toolCallId": id,
                "content": blocks_to_text(content),
            })
        }
    }
}

/// Concatenate all [`ContentBlock::Text`] values.
fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(|b| {
            let ContentBlock::Text { text } = b;
            text.as_str()
        })
        .collect::<Vec<_>>()
        .join("")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::{
        AgentConfig, ChatMessage, ContentBlock, ModelToolCall, ProviderRegistry, Session,
        ToolCallFunction, ToolCallId, ToolName, ToolRegistry, ToolRisk, tool::CancellationToken,
    };
    use rho_test_helpers::{
        FixedResponseTool, MockChatClient, TestProvider, text_events, tool_call_events,
    };
    use std::io::Cursor;

    // ═══════════════════════════════════════════════════════════════════════
    // Test infrastructure
    // ═══════════════════════════════════════════════════════════════════════

    /// Wrapper to make Arc<Mutex<Vec<u8>>> usable as a Write sink.
    struct WriterWrapper(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for WriterWrapper {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .flush()
        }
    }

    fn test_app(client: MockChatClient, registry: ToolRegistry) -> App {
        let session = Session::in_memory(
            "test-model",
            Some("You are a helpful assistant."),
            vec![],
            std::path::Path::new("."),
        )
        .with_token_budget(rho_core::TokenBudget::default());
        let mut providers = ProviderRegistry::new();
        providers.add(Box::new(TestProvider::new("test", client)));
        App {
            session,
            providers,
            active_provider_index: 0,
            registry,
            config: AgentConfig::default(),
            cancel: CancellationToken::new(),
            ext_loader: rho_ext::loader::ExtensionLoader::new(
                rho_core::ExtensionConfig::default(),
                std::path::PathBuf::new(),
                rho_core::denylist::CommandDenylist::default_powershell(),
            ),
            ext_observers: vec![],
            _log_guard: tracing_appender::non_blocking(tracing_appender::rolling::never(
                "logs", "test.log",
            ))
            .1,
        }
    }

    fn echo_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FixedResponseTool {
            name: "echo_tool",
            response: "echo".into(),
            risk: ToolRisk::Read,
        }));
        registry
    }

    fn destructive_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FixedResponseTool {
            name: "destroy_tool",
            response: "destroyed".into(),
            risk: ToolRisk::Destructive,
        }));
        registry
    }

    async fn rpc_run(client: MockChatClient, registry: ToolRegistry, lines: &[&str]) -> Vec<Value> {
        let app = test_app(client, registry);
        let stdin_data = lines.join("\n") + "\n";
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);

        run_rpc_on(app, reader, WriterWrapper(writer_clone))
            .await
            .expect("should not panic");

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        parse_output(&output)
    }

    fn parse_output(bytes: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("valid JSON"))
            .collect()
    }

    fn events_of_type(events: &[Value], method: &str) -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.get("method").and_then(|m| m.as_str()) == Some(method))
            .cloned()
            .collect()
    }

    fn responses(events: &[Value]) -> Vec<Value> {
        events
            .iter()
            .filter(|e| e.get("result").is_some() || e.get("error").is_some())
            .cloned()
            .collect()
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 1. Unit tests for helpers
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn success_response_shape() {
        let resp = success_response(&json!(1), json!({"ok": true}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["ok"], true);
        assert!(!resp.as_object().unwrap().contains_key("error"));
    }

    #[test]
    fn error_response_shape() {
        let resp = error_response(&json!("abc"), INVALID_PARAMS, "missing field");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], "abc");
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("missing")
        );
    }

    #[test]
    fn notification_shape() {
        let notif = notification("agent/start", json!({}));
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "agent/start");
        assert!(!notif.as_object().unwrap().contains_key("id"));
    }

    #[test]
    fn state_name_idle() {
        assert_eq!(state_name(&AgentState::Idle), "idle");
    }

    #[test]
    fn state_name_thinking() {
        assert_eq!(state_name(&AgentState::Thinking), "thinking");
    }

    #[test]
    fn risk_label_read() {
        assert_eq!(risk_label(ToolRisk::Read), "read");
    }

    #[test]
    fn blocks_to_text_empty() {
        assert_eq!(blocks_to_text(&[]), "");
    }

    #[test]
    fn blocks_to_text_single() {
        let blocks = vec![ContentBlock::Text {
            text: "hello".into(),
        }];
        assert_eq!(blocks_to_text(&blocks), "hello");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 2. Protocol validation
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn ready_notification_on_startup() {
        let events = rpc_run(MockChatClient::new(vec![]), echo_registry(), &[""]).await;
        let ready = events_of_type(&events, "ready");
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["jsonrpc"], "2.0");
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &["{not json}"],
        )
        .await;
        let errs: Vec<_> = events.iter().filter(|e| e.get("error").is_some()).collect();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["error"]["code"], PARSE_ERROR);
    }

    #[tokio::test]
    async fn missing_jsonrpc_version_returns_invalid_request() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"method":"getState","id":1}"#],
        )
        .await;
        let errs: Vec<_> = events.iter().filter(|e| e.get("error").is_some()).collect();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["error"]["code"], INVALID_REQUEST);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"unknown","id":1}"#],
        )
        .await;
        let errs: Vec<_> = events.iter().filter(|e| e.get("error").is_some()).collect();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 3. Method: getState
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn get_state_returns_model_and_provider() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"getState","id":1}"#],
        )
        .await;
        let resp = &responses(&events)[0];
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["model"], "test-model");
        assert_eq!(resp["result"]["provider"], "test");
        assert!(resp["result"]["cwd"].is_string());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 4. Method: prompt (text-only)
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn prompt_text_only_event_sequence() {
        let client = MockChatClient::new(vec![text_events("hello world")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"prompt","params":{"message":"say hello"},"id":1}"#],
        )
        .await;

        // Should have: ready notification, agent/start, state/change(s), message/delta, agent/end
        let ready = events_of_type(&events, "ready");
        assert_eq!(ready.len(), 1);

        let starts = events_of_type(&events, "agent/start");
        assert_eq!(starts.len(), 1);

        let ends = events_of_type(&events, "agent/end");
        assert_eq!(ends.len(), 1);

        let resp = &responses(&events)[0];
        assert_eq!(resp["result"]["reply"], "hello world");
    }

    #[tokio::test]
    async fn prompt_empty_message_rejected() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"prompt","params":{"message":""},"id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 5. Method: prompt with tool calls
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn prompt_tool_call_approved() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "echo_tool", "{}"),
            text_events("done"),
        ]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"prompt","params":{"message":"use tool"},"id":1}"#],
        )
        .await;

        let tool_calls = events_of_type(&events, "tool/call");
        assert_eq!(tool_calls.len(), 1);

        let tool_results = events_of_type(&events, "tool/result");
        assert_eq!(tool_results.len(), 1);

        let ends = events_of_type(&events, "agent/end");
        assert_eq!(ends.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 6. Approval flow
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn approval_granted_flow() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "destroy_tool", "{}"),
            text_events("done"),
        ]);
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"jsonrpc":"2.0","method":"prompt","params":{"message":"destroy"},"id":1}"#,
                r#"{"jsonrpc":"2.0","method":"approvalResponse","params":{"approved":true},"id":2}"#,
            ],
        )
        .await;

        let approvals = events_of_type(&events, "approval/request");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0]["params"]["tool"], "destroy_tool");
        assert_eq!(approvals[0]["params"]["risk"], "destructive");

        let tool_results = events_of_type(&events, "tool/result");
        assert_eq!(tool_results.len(), 1);
    }

    #[tokio::test]
    async fn approval_denied_flow() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "destroy_tool", "{}"),
            text_events("ok"),
        ]);
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"jsonrpc":"2.0","method":"prompt","params":{"message":"destroy"},"id":1}"#,
                r#"{"jsonrpc":"2.0","method":"approvalResponse","params":{"approved":false},"id":2}"#,
            ],
        )
        .await;

        let denied = events_of_type(&events, "tool/denied");
        assert_eq!(denied.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 7. Other methods
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn abort_cancels_token() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"abort","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn clear_branches_to_root() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"clear","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["result"], json!({}));
    }

    #[tokio::test]
    async fn get_messages_returns_array() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"getMessages","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["messages"].as_array().is_some());
    }

    #[tokio::test]
    async fn set_model_switches_model() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"setModel","params":{"model":"new-model"},"id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["result"]["model"], "new-model");
    }

    #[tokio::test]
    async fn list_models_returns_array() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"listModels","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["models"].as_array().is_some());
    }

    #[tokio::test]
    async fn get_session_stats_returns_fields() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"getSessionStats","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["contextWindow"].is_number());
        assert!(resp["result"]["estimatedUsed"].is_number());
    }

    #[tokio::test]
    async fn list_sessions_returns_array() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"listSessions","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["sessions"].as_array().is_some());
    }

    #[tokio::test]
    async fn list_extensions_returns_array() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"listExtensions","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["extensions"].as_array().is_some());
    }

    #[tokio::test]
    async fn reload_extensions_returns_counts() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"reloadExtensions","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert!(resp["result"]["added"].is_number());
        assert!(resp["result"]["reloaded"].is_number());
        assert!(resp["result"]["removed"].is_number());
        assert!(resp["result"]["failed"].is_number());
    }

    #[tokio::test]
    async fn compact_returns_response() {
        // Even with a small session, compact should return a response.
        // It may succeed or fail "nothing to compact" — either is fine.
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"compact","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        // Should have either result or error, but both are valid responses
        assert!(resp.get("result").is_some() || resp.get("error").is_some());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 8. Message serialization
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn message_to_json_system() {
        let msg = ChatMessage::System {
            content: vec![ContentBlock::Text {
                text: "Be helpful.".into(),
            }],
        };
        let v = message_to_json(&msg);
        assert_eq!(v["role"], "system");
        assert_eq!(v["content"], "Be helpful.");
    }

    #[test]
    fn message_to_json_user() {
        let msg = ChatMessage::User {
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        };
        let v = message_to_json(&msg);
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hello");
    }

    #[test]
    fn message_to_json_assistant_with_tools() {
        let msg = ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![ModelToolCall {
                id: ToolCallId::new("c1".to_owned()),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: ToolName::new("read_file".to_owned()),
                    arguments: r#"{"path":"a.rs"}"#.into(),
                },
            }],
        };
        let v = message_to_json(&msg);
        assert_eq!(v["role"], "assistant");
        let calls = v["toolCalls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "c1");
        assert_eq!(calls[0]["name"], "read_file");
    }

    #[test]
    fn message_to_json_tool_result() {
        let msg = ChatMessage::Tool {
            tool_call_id: ToolCallId::new("c1".to_owned()),
            content: vec![ContentBlock::Text {
                text: "file contents".into(),
            }],
        };
        let v = message_to_json(&msg);
        assert_eq!(v["role"], "tool");
        assert_eq!(v["toolCallId"], "c1");
        assert_eq!(v["content"], "file contents");
    }
}
