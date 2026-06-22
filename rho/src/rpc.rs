//! Headless JSON-RPC 2.0 mode — transport-agnostic.
//!
//! In RPC mode rho reads JSON-RPC 2.0 requests from a [`Transport`] and
//! writes JSON-RPC 2.0 responses back. Streaming events are delivered as
//! JSON-RPC notifications (no `id` field).
//!
//! The transport is pluggable: [`StdioTransport`] (newline-delimited JSON
//! over stdin/stdout) is the default, but any implementation of the
//! [`Transport`] trait works (WebSocket, Unix socket, TCP, etc.) without
//! changes to the dispatch logic.
//!
//! # Protocol
//!
//! Every request must be a JSON object with `jsonrpc: "2.0"`, a `method` field,
//! optional `params`, and a numeric or string `id` for response correlation.
//!
//! ## Methods (client → rho)
//!
//! | Method             | Params                      | Description                        |
//! |--------------------|-----------------------------|------------------------------------|
//! | `prompt`           | `{message: string}`         | Send a user message to the agent   |
//! | `abort`            | —                           | Cancel the current operation       |
//! | `clear`            | —                           | Clear conversation history         |
//! | `getState`         | —                           | Return model, provider, and cwd  |
//! | `getMessages`      | —                           | Return all messages on active path |
//! | `setModel`         | `{model: string}`          | Switch model (`id` or `provider:id`) |
//! | `listModels`       | —                           | List available models from providers |
//! | `listProviders`    | —                           | List configured providers with reachability |//! | `getSessionStats`  | —                           | Return token budget / usage info   |
//! | `listSessions`     | —                           | List previous sessions for project |
//! | `listExtensions`   | —                           | List loaded extensions and tools   |
//! | `reloadExtensions` | —                           | Reload extensions from disk        |
//! | `compact`          | —                           | Trigger context compaction         |
//! | `resumeSession`    | `{path: string}`            | Resume a previous session from JSONL |
//! | `listTools`        | —                           | List registered tools with schemas |
//!
//! ## Notifications (rho → client)
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
//! an `approvalResponse` method from the transport:
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
use crate::rpc_wire::{
    AgentEndParams, AgentErrorParams, AgentStartParams, ApprovalRequestParams, EmptyResult,
    ExtensionEntry, GetMessagesResult, GetSessionStatsResult, GetStateResult, ListExtensionsResult,
    ListModelsResult, ListProvidersResult, ListSessionsResult, ListToolsResult, MessageDeltaParams,
    ModelEntry, PromptErrorResult, PromptParams, PromptResult, ProviderEntry, ReadyParams,
    ReasoningDeltaParams, ResumeSessionParams, ResumeSessionResult, SessionEntry, SetModelParams,
    SetModelResult, StateChangeParams, ToolCallParams, ToolDeniedParams, ToolEntry,
    ToolResultParams, UsageContextWire, UsageDeltaWire, UsageParams, notification, risk_label,
    state_name,
};
use crate::transport::{ReadResult, StdioTransport, Transport};
use anyhow::Result;
use async_trait::async_trait;
use rho_core::{AgentObserver, ChatMessage, ContentBlock, ModelToolCall, ToolResult};
use serde_json::{Value, json};
use std::io;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

// ── JSON-RPC 2.0 Error codes ───────────────────────────────────────────────────

/// Parse error: Invalid JSON was received.
#[allow(dead_code)]
const PARSE_ERROR: i32 = -32700;
/// Invalid request: The JSON sent is not a valid Request object.
const INVALID_REQUEST: i32 = -32600;
/// Method not found: The method does not exist / is not available.
const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid params: Invalid method parameter(s).
const INVALID_PARAMS: i32 = -32602;
/// Internal error: Internal JSON-RPC error.
const INTERNAL_ERROR: i32 = -32603;

// ── Transport helper ────────────────────────────────────────────────────────────

/// Send a single JSON-RPC message via the transport, discarding I/O errors.
async fn send(transport: &dyn Transport, value: &Value) {
    let _ = transport.write_message(value).await;
}

// ── JSON-RPC response builders ────────────────────────────────────────────────

/// Build a successful JSON-RPC response.
#[allow(clippy::needless_pass_by_value)]
fn success_response(id: &Value, result: impl serde::Serialize) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": serde_json::to_value(result).unwrap_or_default(),
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

// ── RpcObserver ───────────────────────────────────────────────────────────────

/// Forwards agent-loop events to the RPC client as JSON-RPC notifications.
///
/// Now that [`AgentObserver`] is async, the observer can write notifications
/// directly via [`Transport::write_message`], eliminating the `write_sync`
/// hack and `tokio::spawn` workarounds.
struct RpcObserver {
    /// Shared transport handle.
    transport: Arc<dyn Transport>,
}

#[async_trait]
impl AgentObserver for RpcObserver {
    async fn on_state_change(&self, state: rho_core::AgentState) {
        let _ = self
            .transport
            .write_message(&notification(
                "state/change",
                &StateChangeParams {
                    state: state_name(&state).into(),
                },
            ))
            .await;
    }

    async fn on_text_delta(&self, delta: &str) {
        let _ = self
            .transport
            .write_message(&notification(
                "message/delta",
                &MessageDeltaParams {
                    delta: delta.into(),
                },
            ))
            .await;
    }

    async fn on_reasoning_delta(&self, delta: &str) {
        let _ = self
            .transport
            .write_message(&notification(
                "reasoning/delta",
                &ReasoningDeltaParams {
                    delta: delta.into(),
                },
            ))
            .await;
    }

    async fn on_tool_call(&self, name: &str, arguments: &str) {
        let _ = self
            .transport
            .write_message(&notification(
                "tool/call",
                &ToolCallParams {
                    name: name.into(),
                    arguments: arguments.into(),
                },
            ))
            .await;
    }

    async fn on_tool_result(&self, name: &str, result: &ToolResult) {
        let _ = self
            .transport
            .write_message(&notification(
                "tool/result",
                &ToolResultParams {
                    name: name.into(),
                    is_error: result.is_error,
                    output: result.output.clone(),
                },
            ))
            .await;
    }

    async fn on_tool_denied(&self, name: &str) {
        let _ = self
            .transport
            .write_message(&notification(
                "tool/denied",
                &ToolDeniedParams { name: name.into() },
            ))
            .await;
    }

    async fn on_usage(
        &self,
        iteration: u32,
        usage: &rho_core::IterationUsage,
        context: &rho_core::session::ContextStats,
    ) {
        let _ = self
            .transport
            .write_message(&notification(
                "usage",
                &UsageParams {
                    iteration,
                    usage: UsageDeltaWire {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cached_tokens: usage.cached_tokens,
                        cost: usage.cost,
                        request_count: usage.request_count,
                    },
                    context: UsageContextWire {
                        estimated_used: context.estimated_used as u64,
                        context_window: context.context_window as u64,
                        completion_reserve: context.completion_reserve as u64,
                        utilization_percent: context.utilization_percent(),
                    },
                },
            ))
            .await;
    }
}

// ── RpcApprovalGate ───────────────────────────────────────────────────────────

/// Writes an `approval/request` notification and reads an `approvalResponse`
/// request from the transport.
struct RpcApprovalGate {
    /// Shared transport handle.
    transport: Arc<dyn Transport>,
}

#[async_trait]
impl rho_core::ApprovalGate for RpcApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: rho_core::ToolRisk) -> bool {
        let _ = self
            .transport
            .write_message(&notification(
                "approval/request",
                &ApprovalRequestParams {
                    tool: call.function.name.to_string(),
                    arguments: call.function.arguments.clone(),
                    risk: risk_label(risk).into(),
                },
            ))
            .await;

        match self.transport.read_message().await {
            ReadResult::Message(v) => v["params"]["approved"].as_bool().unwrap_or(false),
            _ => false,
        }
    }
}

// ── run_rpc / run_rpc_on ──────────────────────────────────────────────────────

/// Run the agent in headless JSON-RPC mode with real stdin/stdout.
///
/// # Errors
///
/// Returns an error if the transport fails unexpectedly.
pub async fn run_rpc(app: App) -> Result<()> {
    let transport: Arc<dyn Transport> = Arc::new(StdioTransport::new(
        io::BufReader::new(io::stdin()),
        io::stdout(),
    ));
    run_rpc_on(app, transport).await
}

/// Core JSON-RPC loop — transport-agnostic.
///
/// Emits a `ready` notification, then reads JSON-RPC requests from the
/// transport one message at a time. Each request is dispatched to the
/// appropriate handler. Exits cleanly on transport disconnect.
///
/// # Errors
///
/// Returns an error if the transport fails unexpectedly.
pub(crate) async fn run_rpc_on(mut app: App, transport: Arc<dyn Transport>) -> Result<()> {
    send(&*transport, &notification("ready", &ReadyParams {})).await;
    info!("RPC server started, emitting ready notification");

    loop {
        let request = match transport.read_message().await {
            ReadResult::Message(v) => v,
            ReadResult::ParseError(e) => {
                warn!(error = %e, "JSON-RPC parse error");
                send(
                    &*transport,
                    &error_response(&Value::Null, PARSE_ERROR, &format!("Parse error: {e}")),
                )
                .await;
                continue;
            }
            ReadResult::Eof => {
                debug!("transport EOF received, shutting down");
                app.session.close("transport EOF");
                break;
            }
        };

        // Extract required fields.
        let jsonrpc = request.get("jsonrpc").and_then(|v| v.as_str());
        let method = request.get("method").and_then(|v| v.as_str());
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Validate jsonrpc version.
        if jsonrpc != Some("2.0") {
            send(
                &*transport,
                &error_response(&id, INVALID_REQUEST, "Invalid JSON-RPC version"),
            )
            .await;
            continue;
        }

        // Validate method.
        let Some(method) = method else {
            send(
                &*transport,
                &error_response(&id, INVALID_REQUEST, "Missing method field"),
            )
            .await;
            continue;
        };

        // Dispatch.
        dispatch_request(&mut app, method, params, &id, Arc::clone(&transport)).await;
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
    transport: Arc<dyn Transport>,
) {
    debug!(method = %method, id = %id, "dispatching RPC request");
    match method {
        "prompt" => match serde_json::from_value::<PromptParams>(params) {
            Ok(p) if !p.message.is_empty() => {
                handle_prompt(app, p, id, Arc::clone(&transport)).await;
            }
            _ => {
                send(
                    &*transport,
                    &error_response(
                        id,
                        INVALID_PARAMS,
                        "prompt requires a non-empty 'message' param",
                    ),
                )
                .await;
            }
        },
        "abort" => {
            app.cancel.cancel();
            send(&*transport, &success_response(id, EmptyResult {})).await;
        }
        "clear" => handle_clear(app, id, &*transport).await,
        "getState" => handle_get_state(app, id, &*transport).await,
        "getMessages" => handle_get_messages(app, id, &*transport).await,
        "setModel" => match serde_json::from_value::<SetModelParams>(params) {
            Ok(p) if !p.model.is_empty() => handle_set_model(app, p, id, &*transport).await,
            _ => {
                send(
                    &*transport,
                    &error_response(
                        id,
                        INVALID_PARAMS,
                        "setModel requires a non-empty 'model' param",
                    ),
                )
                .await;
            }
        },
        "listModels" => handle_list_models(app, id, &*transport).await,
        "listProviders" => handle_list_providers(app, id, &*transport).await,
        "getSessionStats" => handle_get_session_stats(app, id, &*transport).await,
        "listSessions" => handle_list_sessions(app, id, &*transport).await,
        "listExtensions" => handle_list_extensions(app, id, &*transport).await,
        "reloadExtensions" => handle_reload_extensions(app, id, &*transport).await,
        "compact" => handle_compact(app, id, &*transport).await,
        "resumeSession" => match serde_json::from_value::<ResumeSessionParams>(params) {
            Ok(p) if !p.path.is_empty() => handle_resume_session(app, p, id, &*transport).await,
            _ => {
                send(
                    &*transport,
                    &error_response(
                        id,
                        INVALID_PARAMS,
                        "resumeSession requires a non-empty 'path' param",
                    ),
                )
                .await;
            }
        },
        "listTools" => handle_list_tools(app, id, &*transport).await,
        "approvalResponse" => {
            // Handled synchronously during approval flow, but if we see it here,
            // acknowledge it (shouldn't normally happen outside approval flow).
            send(&*transport, &success_response(id, EmptyResult {})).await;
        }
        _ => {
            debug!(method = %method, "unknown RPC method");
            send(
                &*transport,
                &error_response(id, METHOD_NOT_FOUND, &format!("Method not found: {method}")),
            )
            .await;
        }
    }
}

// ── Request handlers ─────────────────────────────────────────────────────────

/// Run one agent turn for the user message.
async fn handle_prompt(
    app: &mut App,
    params: PromptParams,
    id: &Value,
    transport: Arc<dyn Transport>,
) {
    let message = params.message;

    send(
        &*transport,
        &notification("agent/start", &AgentStartParams {}),
    )
    .await;

    debug!(
        message_len = message.len(),
        model = %app.session.model(),
        "agent turn started"
    );
    let turn_start = Instant::now();

    let observer = RpcObserver {
        transport: Arc::clone(&transport),
    };
    let composite = CompositeObserver::new({
        let mut obs: Vec<&dyn AgentObserver> = vec![&observer];
        for ext_obs in &app.ext_observers {
            obs.push(ext_obs);
        }
        obs
    });
    let gate = RpcApprovalGate {
        transport: Arc::clone(&transport),
    };
    let client = app.active_provider().clone_boxed_service();
    let compaction_client = if app.config.compaction_mode == "llm" {
        Some(std::sync::Arc::from(
            app.active_provider().clone_boxed_service(),
        ))
    } else {
        None
    };
    let loop_params = rho_core::LoopParams {
        client: client.as_ref(),
        registry: &app.registry,
        config: &app.config,
        cancel: app.cancel.clone(),
        gate: &gate,
        observer: &composite,
        compaction_client,
    };
    match run_agent_turn(&mut app.session, &message, &loop_params).await {
        TurnResult::Done(result) => {
            let reply = result.reply.clone();
            let end_params = AgentEndParams::from(result);
            info!(
                message_len = message.len(),
                reply_len = reply.len(),
                duration_ms = end_params.duration_ms,
                iterations = end_params.iterations,
                model = %app.session.model(),
                "agent turn completed"
            );
            send(&*transport, &notification("agent/end", &end_params)).await;
            send(&*transport, &success_response(id, PromptResult { reply })).await;
        }
        TurnResult::Error(e) => {
            let elapsed = turn_start.elapsed();
            info!(
                message_len = message.len(),
                duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                model = %app.session.model(),
                error = %e,
                "agent turn failed"
            );
            send(
                &*transport,
                &notification("agent/error", &AgentErrorParams { error: e.clone() }),
            )
            .await;
            send(
                &*transport,
                &success_response(id, PromptErrorResult { error: e }),
            )
            .await;
        }
    }
}

/// Return the current model, provider, and working directory.
async fn handle_get_state(app: &App, id: &Value, transport: &dyn Transport) {
    send(
        transport,
        &success_response(
            id,
            GetStateResult {
                model: app.session.model().to_owned(),
                provider: app.active_provider().name().to_owned(),
                cwd: app.session.header().cwd.to_string_lossy().into_owned(),
            },
        ),
    )
    .await;
}

/// Return all messages on the current session path.
async fn handle_get_messages(app: &App, id: &Value, transport: &dyn Transport) {
    let messages: Vec<Value> = app
        .session
        .path_messages()
        .iter()
        .map(message_to_json)
        .collect();
    send(
        transport,
        &success_response(id, GetMessagesResult { messages }),
    )
    .await;
}

/// Switch the active model.
///
/// Delegates to [`App::set_model`]. On rejection (unknown model or unknown
/// provider) emits a JSON-RPC `INVALID_PARAMS` error with a user-actionable
/// message and leaves the session untouched — the frontend surfaces this as a
/// failed switch rather than falsely reporting success.
async fn handle_set_model(
    app: &mut App,
    params: SetModelParams,
    id: &Value,
    transport: &dyn Transport,
) {
    let spec = &params.model;
    let old_model = app.session.model().to_owned();
    let old_provider = app.active_provider().name().to_owned();
    match app.set_model(spec).await {
        Ok(()) => {
            let provider = app.active_provider().name().to_owned();
            let model = app.session.model().to_owned();
            info!(
                old_model = %old_model,
                old_provider = %old_provider,
                new_model = %model,
                new_provider = %provider,
                spec = %spec,
                "model switched"
            );
            send(
                transport,
                &success_response(id, SetModelResult { model, provider }),
            )
            .await;
        }
        Err(e) => {
            warn!(spec = %spec, error = ?e, "model switch rejected");
            send(transport, &error_response(id, INVALID_PARAMS, &e.message())).await;
        }
    }
}

/// Return token budget and context-window usage statistics.
async fn handle_get_session_stats(app: &App, id: &Value, transport: &dyn Transport) {
    let stats = app.session.context_stats();
    let usage = app.session.api_usage();
    let result = GetSessionStatsResult::from(&stats).with_api_usage(usage);
    send(transport, &success_response(id, result)).await;
}

/// Trigger context compaction.
/// Trigger context compaction.
async fn handle_compact(app: &mut App, id: &Value, transport: &dyn Transport) {
    info!("compaction triggered via RPC");
    match app.compact().await {
        Ok(()) => send(transport, &success_response(id, EmptyResult {})).await,
        Err(e) => {
            warn!(error = %e, "compaction failed");
            send(
                transport,
                &error_response(id, INTERNAL_ERROR, &e.to_string()),
            )
            .await;
        }
    }
}

/// Clear conversation history by branching back to system message.
async fn handle_clear(app: &mut App, id: &Value, transport: &dyn Transport) {
    info!("session cleared");
    let path = app.session.path_to_root();
    if let Some(root_entry) = path.last() {
        let root_id = root_entry.id.clone();
        let _ = app.session.branch_to(&root_id);
        send(transport, &success_response(id, EmptyResult {})).await;
    } else {
        send(
            transport,
            &error_response(id, INTERNAL_ERROR, "No root entry to clear to"),
        )
        .await;
    }
}

/// List available models from all providers.
async fn handle_list_models(app: &App, id: &Value, transport: &dyn Transport) {
    let all = app.providers.list_all_models().await;
    let models: Vec<ModelEntry> = all
        .iter()
        .map(|(provider, info)| ModelEntry {
            id: info.id.clone(),
            provider: provider.to_string(),
        })
        .collect();
    send(
        transport,
        &success_response(id, ListModelsResult { models }),
    )
    .await;
}

/// List configured providers with reachability status.
async fn handle_list_providers(app: &App, id: &Value, transport: &dyn Transport) {
    let providers = app.providers.list_providers().await;
    let active = app.active_provider().name();
    let list: Vec<ProviderEntry> = providers
        .iter()
        .map(|p| ProviderEntry {
            name: p.name.clone(),
            is_external: p.is_external,
            reachable: p.reachable,
            active: p.name == active,
        })
        .collect();
    send(
        transport,
        &success_response(id, ListProvidersResult { providers: list }),
    )
    .await;
}

/// List previous sessions for this project.
async fn handle_list_sessions(app: &App, id: &Value, transport: &dyn Transport) {
    let cwd = app.session.header().cwd.clone();
    let sessions = rho_core::list_sessions(&cwd);
    let list: Vec<SessionEntry> = sessions
        .iter()
        .map(|meta| {
            let mtime = meta
                .mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let size_kb = std::fs::metadata(&meta.path).map_or(0, |m| m.len() / 1024);
            SessionEntry {
                path: meta.path.to_string_lossy().into_owned(),
                mtime_secs: mtime,
                size_kb,
                entry_count: meta.entry_count as u64,
            }
        })
        .collect();
    send(
        transport,
        &success_response(id, ListSessionsResult { sessions: list }),
    )
    .await;
}

/// List loaded extensions and their tools.
async fn handle_list_extensions(app: &App, id: &Value, transport: &dyn Transport) {
    let extensions: Vec<ExtensionEntry> = app
        .ext_loader
        .extension_tools()
        .into_iter()
        .map(|(name, tools)| ExtensionEntry { name, tools })
        .collect();
    send(
        transport,
        &success_response(id, ListExtensionsResult { extensions }),
    )
    .await;
}

/// Resume a previous session from a JSONL file path.
///
/// Replaces the current session with the loaded one, restoring the
/// model, tools, and token budget from the current app configuration.
/// The opened session continues appending to the same JSONL file.
async fn handle_resume_session(
    app: &mut App,
    params: ResumeSessionParams,
    id: &Value,
    transport: &dyn Transport,
) {
    let path = std::path::PathBuf::from(&params.path);
    match rho_core::Session::open(&path) {
        Ok(mut session) => {
            let old_model = app.session.model().to_owned();
            session.set_model(&old_model);
            session.set_tools(app.registry.tool_definitions());
            session.set_token_budget(app.session.token_budget());
            let session_cwd = session.header().cwd.clone();
            let current_cwd = app.session.header().cwd.clone();
            if session_cwd != current_cwd {
                warn!(
                    session_cwd = %session_cwd.display(),
                    current_cwd = %current_cwd.display(),
                    "resumed session CWD differs from current working directory"
                );
            }
            app.session = session;
            info!(
                path = %path.display(),
                model = %old_model,
                "session resumed via RPC"
            );
            send(
                transport,
                &success_response(
                    id,
                    ResumeSessionResult {
                        path: params.path,
                        model: old_model,
                        cwd: app.session.header().cwd.to_string_lossy().into_owned(),
                        entry_count: app.session.entry_count() as u64,
                    },
                ),
            )
            .await;
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "failed to resume session");
            send(
                transport,
                &error_response(id, INTERNAL_ERROR, &format!("failed to open session: {e}")),
            )
            .await;
        }
    }
}

/// List all registered tools with their names, descriptions, risk levels,
/// and parameter schemas.
async fn handle_list_tools(app: &App, id: &Value, transport: &dyn Transport) {
    let tools: Vec<ToolEntry> = app
        .registry
        .list()
        .iter()
        .map(|t| ToolEntry {
            name: t.name().to_string(),
            description: t.description().to_owned(),
            risk: risk_label(t.risk()).to_owned(),
            parameters: t.parameters_schema(),
        })
        .collect();
    send(transport, &success_response(id, ListToolsResult { tools })).await;
}

/// Reload extensions from disk.
async fn handle_reload_extensions(app: &mut App, id: &Value, transport: &dyn Transport) {
    let dirs = crate::app::extension_dirs(&app.session.header().cwd);
    let fresh_config = rho_core::ConfigLoader::load(&app.session.header().cwd).unwrap_or_default();
    app.ext_loader.set_config(fresh_config.extensions);
    match app.ext_loader.reload(&dirs, &mut app.registry).await {
        Ok(report) => {
            info!(
                added = report.added.len(),
                reloaded = report.reloaded.len(),
                removed = report.removed.len(),
                failed = report.failed.len(),
                "extensions reloaded"
            );
            app.ext_observers = app.ext_loader.build_observers();
            send(
                transport,
                &success_response(
                    id,
                    json!({
                        "added": report.added.len(),
                        "reloaded": report.reloaded.len(),
                        "removed": report.removed.len(),
                        "failed": report.failed.len(),
                    }),
                ),
            )
            .await;
        }
        Err(e) => {
            warn!(error = %e, "extension reload failed");
            send(
                transport,
                &error_response(id, INTERNAL_ERROR, &e.to_string()),
            )
            .await;
        }
    }
}

// ── Message serialization ─────────────────────────────────────────────────

/// Serialize a [`ChatMessage`] to a JSON value.
///
/// Kept as a custom serializer (option (b)) because the per-role shapes
/// are complex and don't map to a single struct without coupling `rho-core`
/// to the wire format.
pub(crate) fn message_to_json(msg: &ChatMessage) -> Value {
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
        AgentConfig, AgentState, ChatMessage, ContentBlock, ModelToolCall, ProviderRegistry,
        Session, ToolCallFunction, ToolCallId, ToolName, ToolRegistry, ToolRisk,
        tool::CancellationToken,
    };
    use rho_test_helpers::{
        FixedResponseTool, MockChatClient, TestProvider, text_events, tool_call_events,
    };
    use std::io::{Cursor, Write};

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
        let transport: Arc<dyn Transport> =
            Arc::new(StdioTransport::new(reader, WriterWrapper(writer_clone)));

        run_rpc_on(app, transport).await.expect("should not panic");

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        parse_output(&output)
    }

    /// Like [`rpc_run`] but with an explicit provider registry, for tests that
    /// need custom provider/model topology.
    async fn rpc_run_with_providers(
        providers: ProviderRegistry,
        registry: ToolRegistry,
        lines: &[&str],
    ) -> Vec<Value> {
        let app = test_app_with_providers(providers, registry);
        let stdin_data = lines.join("\n") + "\n";
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);
        let transport: Arc<dyn Transport> =
            Arc::new(StdioTransport::new(reader, WriterWrapper(writer_clone)));

        run_rpc_on(app, transport).await.expect("should not panic");

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        parse_output(&output)
    }

    fn test_app_with_providers(providers: ProviderRegistry, registry: ToolRegistry) -> App {
        let session = Session::in_memory(
            "test-model",
            Some("You are a helpful assistant."),
            vec![],
            std::path::Path::new("."),
        )
        .with_token_budget(rho_core::TokenBudget::default());
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
        let notif =
            crate::rpc_wire::notification("agent/start", &crate::rpc_wire::AgentStartParams {});
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
        // The default TestProvider only advertises "mock-model"; advertise
        // "new-model" too so the switch resolves via /v1/models discovery.
        let providers = {
            let mut providers = ProviderRegistry::new();
            providers.add(Box::new(
                TestProvider::new("test", MockChatClient::new(vec![]))
                    .with_models(["mock-model", "new-model"]),
            ));
            providers
        };
        let events = rpc_run_with_providers(
            providers,
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"setModel","params":{"model":"new-model"},"id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["result"]["model"], "new-model");
        assert_eq!(resp["result"]["provider"], "test");
    }

    #[tokio::test]
    async fn set_model_switches_provider() {
        // Two providers: alpha serves alpha-1/alpha-2, beta serves beta-1/beta-2.
        let providers = {
            let mut providers = ProviderRegistry::new();
            providers.add(Box::new(
                TestProvider::new("alpha", MockChatClient::new(vec![]))
                    .with_models(["alpha-1", "alpha-2"]),
            ));
            providers.add(Box::new(
                TestProvider::new("beta", MockChatClient::new(vec![]))
                    .with_models(["beta-1", "beta-2"]),
            ));
            providers
        };

        let app = test_app_with_providers(providers, echo_registry());

        // Send setModel + getState in a single stream.
        let msg1 = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "setModel",
            "params": {"model": "beta-1"},
            "id": 1
        }))
        .unwrap();
        let msg2 = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "getState",
            "id": 2
        }))
        .unwrap();
        let stdin_data = format!(
            "{msg1}
{msg2}
"
        );
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);
        let transport: Arc<dyn Transport> =
            Arc::new(StdioTransport::new(reader, WriterWrapper(writer_clone)));

        run_rpc_on(app, transport).await.expect("should not panic");

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        let events = parse_output(&output);
        let all_resps = responses(&events);

        // setModel response: model switched to beta-1, provider switched to beta.
        assert_eq!(all_resps[0]["result"]["model"], "beta-1");
        assert_eq!(all_resps[0]["result"]["provider"], "beta");

        // getState confirms the switch persisted.
        assert_eq!(all_resps[1]["result"]["provider"], "beta");
        assert_eq!(all_resps[1]["result"]["model"], "beta-1");
    }

    #[tokio::test]
    async fn set_model_rejects_unknown_model() {
        // Provider test only serves mock-model; "unknown-model" is not
        // advertised, so the switch must be rejected.
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[
                r#"{"jsonrpc":"2.0","method":"setModel","params":{"model":"unknown-model"},"id":1}"#,
                r#"{"jsonrpc":"2.0","method":"getState","id":2}"#,
            ],
        )
        .await;

        let resps = responses(&events);

        // setModel response must be a JSON-RPC error, not a success.
        assert!(
            resps[0].get("error").is_some(),
            "expected an error response"
        );
        let message = resps[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("unknown-model"),
            "error message should name the rejected model: {message}"
        );

        // getState should confirm the model and provider are unchanged.
        assert_eq!(resps[1]["result"]["provider"], "test");
        assert_eq!(resps[1]["result"]["model"], "test-model");
    }

    #[tokio::test]
    async fn set_model_rejects_unknown_provider() {
        // `provider:model` with a provider that isn't configured must be
        // rejected, even though the syntax bypasses model discovery.
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[
                r#"{"jsonrpc":"2.0","method":"setModel","params":{"model":"nope:mock-model"},"id":1}"#,
                r#"{"jsonrpc":"2.0","method":"getState","id":2}"#,
            ],
        )
        .await;

        let resps = responses(&events);

        assert!(
            resps[0].get("error").is_some(),
            "expected an error response"
        );
        let message = resps[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("nope"),
            "error message should name the rejected provider: {message}"
        );

        // Unchanged.
        assert_eq!(resps[1]["result"]["provider"], "test");
        assert_eq!(resps[1]["result"]["model"], "test-model");
    }

    #[tokio::test]
    async fn set_model_explicit_provider_trusts_model_id() {
        // `provider:model` with a *known* provider skips discovery and trusts
        // the user's model id, even if it isn't advertised via /v1/models.
        let providers = {
            let mut providers = ProviderRegistry::new();
            providers.add(Box::new(
                TestProvider::new("alpha", MockChatClient::new(vec![])).with_models(["alpha-1"]),
            ));
            providers.add(Box::new(
                TestProvider::new("beta", MockChatClient::new(vec![])).with_models(["beta-1"]),
            ));
            providers
        };
        let events = rpc_run_with_providers(
            providers,
            echo_registry(),
            &[
                r#"{"jsonrpc":"2.0","method":"setModel","params":{"model":"beta:beta-unlisted"},"id":1}"#,
                r#"{"jsonrpc":"2.0","method":"getState","id":2}"#,
            ],
        )
        .await;

        let resps = responses(&events);

        // Success: provider switched to beta, model string accepted verbatim.
        assert_eq!(resps[0]["result"]["model"], "beta-unlisted");
        assert_eq!(resps[0]["result"]["provider"], "beta");
        assert_eq!(resps[1]["result"]["model"], "beta-unlisted");
        assert_eq!(resps[1]["result"]["provider"], "beta");
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
        assert!(resp["result"]["apiUsage"].is_object());
        assert_eq!(resp["result"]["apiUsage"]["totalInputTokens"], 0);
        assert_eq!(resp["result"]["apiUsage"]["totalOutputTokens"], 0);
        assert_eq!(resp["result"]["apiUsage"]["totalCachedTokens"], 0);
        assert_eq!(resp["result"]["apiUsage"]["requestCount"], 0);
        // The phase breakdown is always present on the wire. Buckets are
        // integers (possibly zero, possibly populated if the seed session
        // has classified entries); assert structure rather than specific
        // values so this isn't coupled to the seed.
        let phase = &resp["result"]["phaseTokens"];
        for key in [
            "exploration",
            "execution",
            "verification",
            "conclusion",
            "unclassified",
        ] {
            assert!(
                phase[key].is_i64(),
                "phaseTokens.{key} should be an integer, got {}",
                phase[key]
            );
        }
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

    // ═══════════════════════════════════════════════════════════════════════
    // 9. listTools
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn list_tools_returns_registered_tools() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"listTools","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["error"], Value::Null, "no error: {resp}");
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo_tool");
        assert_eq!(tools[0]["risk"], "read");
        assert!(tools[0]["parameters"].is_object());
    }

    #[tokio::test]
    async fn list_tools_destructive_shows_risk() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            destructive_registry(),
            &[r#"{"jsonrpc":"2.0","method":"listTools","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "destroy_tool");
        assert_eq!(tools[0]["risk"], "destructive");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 10. resumeSession
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn resume_session_missing_path_returns_error() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"resumeSession","id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn resume_session_nonexistent_file_returns_error() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"jsonrpc":"2.0","method":"resumeSession","params":{"path":"/tmp/nonexistent_rho_session.jsonl"},"id":1}"#],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["error"]["code"], INTERNAL_ERROR);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("failed to open session")
        );
    }

    #[tokio::test]
    async fn resume_session_valid_file_returns_session_info() {
        // Create a real persisted session, then resume it via RPC.
        let dir = std::env::temp_dir().join(format!("rho_test_resume_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Create a session that writes to disk. Use Session::new with a
        // known cwd so the save path lands in our temp dir.
        let mut session = Session::new("test-model", Some("system prompt"), vec![], &dir);
        session.flush().unwrap();
        let save_path = session.save_path().unwrap().to_path_buf();

        let path_str = save_path.display().to_string().replace('\\', "/");

        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[&format!(
                r#"{{"jsonrpc":"2.0","method":"resumeSession","params":{{"path":"{path_str}"}},"id":1}}"#,
            )],
        )
        .await;

        let resp = &responses(&events)[0];
        assert_eq!(resp["error"], Value::Null, "no error: {resp}");
        assert_eq!(resp["result"]["entryCount"], 1); // system prompt entry
        assert_eq!(resp["result"]["model"], "test-model");
        assert!(resp["result"]["path"].as_str().unwrap().contains(".jsonl"));
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

    // ═══════════════════════════════════════════════════════════════════════
    // Memory tool integration
    // ═══════════════════════════════════════════════════════════════════════

    async fn memory_registry() -> ToolRegistry {
        let mem = rho_memory::Memory::open_in_memory()
            .await
            .expect("memory db");
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(rho_tools::MemoryTool::new(std::sync::Arc::new(
            mem,
        ))));
        registry
    }

    #[tokio::test]
    async fn prompt_memory_store_and_reply() {
        // Model calls memory store, gets result, then replies with text.
        let store_args = serde_json::json!({
            "operation": "store",
            "title": "Test Doc",
            "content": "important fact",
            "tags": ["test"]
        });
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "memory", serde_json::to_string(&store_args).unwrap()),
            text_events("stored"),
        ]);
        let events = rpc_run(
            client,
            memory_registry().await,
            &[serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "prompt",
                "params": {"message": "remember this"},
                "id": 1
            }))
            .unwrap()
            .as_str()],
        )
        .await;

        let tool_calls = events_of_type(&events, "tool/call");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["params"]["name"], "memory");

        let tool_results = events_of_type(&events, "tool/result");
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0]["params"]["is_error"], false);

        let ends = events_of_type(&events, "agent/end");
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0]["params"]["reply"], "stored");
    }

    #[tokio::test]
    async fn prompt_memory_search_after_store() {
        // First turn: model stores a doc. Second turn: model searches and gets it back.
        let store_args = serde_json::json!({
            "operation": "store",
            "title": "Architecture",
            "content": "rho uses a layered architecture with rho-core as the kernel",
            "tags": ["architecture", "rho"]
        });
        let search_args = serde_json::json!({
            "operation": "search",
            "query": "layered architecture"
        });
        let client = MockChatClient::new(vec![
            // Turn 1: store
            tool_call_events("c1", "memory", serde_json::to_string(&store_args).unwrap()),
            text_events("saved"),
            // Turn 2: search
            tool_call_events("c2", "memory", serde_json::to_string(&search_args).unwrap()),
            text_events("found it"),
        ]);
        let lines: Vec<String> = vec![
            serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "prompt",
                "params": {"message": "remember the architecture"},
                "id": 1
            }))
            .unwrap(),
            serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "prompt",
                "params": {"message": "what architecture does rho use"},
                "id": 2
            }))
            .unwrap(),
        ];
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let events = rpc_run(client, memory_registry().await, &line_refs).await;

        // Two prompts → two agent/end notifications
        let ends = events_of_type(&events, "agent/end");
        assert_eq!(ends.len(), 2);

        // Both turns should have tool calls and results
        let tool_results = events_of_type(&events, "tool/result");
        assert_eq!(tool_results.len(), 2);
        // All tool results should succeed
        for r in &tool_results {
            assert_eq!(r["params"]["is_error"], false);
        }
    }
}
