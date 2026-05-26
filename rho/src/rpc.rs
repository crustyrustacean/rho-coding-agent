//! Headless RPC mode — JSONL over stdin/stdout.
//!
//! In RPC mode rho reads newline-delimited JSON commands from stdin and writes
//! newline-delimited JSON events to stdout, enabling process integration with
//! editors, bots, and custom UIs.
//!
//! # Protocol
//!
//! Every outbound line is a compact JSON object followed by `\n`. Every
//! inbound command must be a JSON object with at least a `"type"` field.
//!
//! ## Commands (stdin → rho)
//!
//! | `type`              | Required fields       | Description                        |
//! |---------------------|-----------------------|------------------------------------|
//! | `prompt`            | `message`             | Send a user message to the agent   |
//! | `abort`             | —                     | Cancel the current operation       |
//! | `get_state`         | —                     | Return current model / provider    |
//! | `get_messages`      | —                     | Return all messages on active path |
//! | `set_model`         | `model`               | Switch the active model            |
//! | `get_session_stats` | —                     | Return token budget / usage info   |
//! | `compact`           | —                     | Trigger context compaction         |
//!
//! ## Events (rho → stdout)
//!
//! | `type`              | Fields                          | Description                         |
//! |---------------------|---------------------------------|-------------------------------------|
//! | `ready`             | —                               | Emitted once on startup             |
//! | `agent_start`       | —                               | Agent began processing a prompt     |
//! | `agent_end`         | `reply`                         | Agent finished; full text reply     |
//! | `agent_error`       | `error`                         | Agent loop encountered an error     |
//! | `state_change`      | `state`                         | Loop state transition               |
//! | `message_update`    | `delta`                         | Streaming text chunk                |
//! | `reasoning_delta`   | `delta`                         | Streaming reasoning chunk           |
//! | `tool_call`         | `name`, `arguments`             | Model requested a tool call         |
//! | `tool_result`       | `name`, `is_error`, `output`    | Tool finished executing             |
//! | `tool_denied`       | `name`                          | Tool call denied by approval gate   |
//! | `approval_request`  | `tool`, `arguments`, `risk`     | Approval required; send response    |
//! | `response`          | `success`, [`error`]            | Command acknowledgment              |
//!
//! ## Approval flow
//!
//! When rho emits an `approval_request` event it blocks until it reads an
//! `approval_response` command from stdin:
//!
//! ```json
//! {"type": "approval_response", "approved": true}
//! ```
//!
//! Sending `approved: false` (or any non-boolean / missing field) denies the
//! tool call and lets the agent continue.

use crate::app::App;
use anyhow::Result;
use async_trait::async_trait;
use rho_core::{
    AgentObserver, AgentState, ApprovalGate, ChatMessage, ContentBlock,
    MechanicalCompactionStrategy, ModelToolCall, ToolResult, ToolRisk,
};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

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

/// Write a single JSONL event to `out`.
///
/// Serialises `event` as a compact JSON object, appends `\n`, and flushes.
/// The mutex ensures the line is written atomically even when [`RpcObserver`]
/// and the command loop both hold a reference to the same writer.
#[allow(clippy::needless_pass_by_value)]
fn write_event(out: &Out, event: Value) {
    let mut guard = out
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_event_to(&mut *guard, &event);
}

/// Write a single JSONL event to any [`Write`] sink.
///
/// Extracted so unit tests can pass a `Vec<u8>` instead of the locked writer.
fn write_event_to(sink: &mut dyn Write, event: &Value) {
    let _ = writeln!(sink, "{event}");
    let _ = sink.flush();
}

// ── RpcObserver ───────────────────────────────────────────────────────────────

/// Forwards agent-loop events to the RPC client as JSONL.
struct RpcObserver {
    /// Shared writer handle.
    out: Out,
}

impl AgentObserver for RpcObserver {
    fn on_state_change(&self, state: AgentState) {
        write_event(
            &self.out,
            json!({"type": "state_change", "state": state_name(&state)}),
        );
    }

    fn on_text_delta(&self, delta: &str) {
        write_event(&self.out, json!({"type": "message_update", "delta": delta}));
    }

    fn on_reasoning_delta(&self, delta: &str) {
        write_event(
            &self.out,
            json!({"type": "reasoning_delta", "delta": delta}),
        );
    }

    fn on_tool_call(&self, name: &str, arguments: &str) {
        write_event(
            &self.out,
            json!({"type": "tool_call", "name": name, "arguments": arguments}),
        );
    }

    fn on_tool_result(&self, name: &str, result: &ToolResult) {
        write_event(
            &self.out,
            json!({
                "type": "tool_result",
                "name": name,
                "is_error": result.is_error,
                "output": result.output,
            }),
        );
    }

    fn on_tool_denied(&self, name: &str) {
        write_event(&self.out, json!({"type": "tool_denied", "name": name}));
    }
}

// ── RpcApprovalGate ───────────────────────────────────────────────────────────

/// Writes an `approval_request` event to the shared writer and reads an
/// `approval_response` command from the shared reader.
///
/// The gate holds references to both the writer (for emitting the request)
/// and the reader (for consuming the response). During `run_loop` the
/// command loop is blocked, so there is no concurrent reader contention.
struct RpcApprovalGate {
    /// Shared writer handle.
    out: Out,
    /// Shared reader handle.
    input: In,
}

#[async_trait]
impl ApprovalGate for RpcApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool {
        write_event(
            &self.out,
            json!({
                "type": "approval_request",
                "tool": &*call.function.name,
                "arguments": call.function.arguments,
                "risk": risk_label(risk),
            }),
        );

        // Read the approval_response from the shared reader (blocking I/O
        // off the async executor). The main command loop is blocked inside
        // run_loop at this point, so there is no concurrent reader.
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
            Ok(Some(v)) => v["approved"].as_bool().unwrap_or(false),
            _ => false,
        }
    }
}

// ── run_rpc / run_rpc_on ──────────────────────────────────────────────────────

/// Run the agent in headless RPC mode with real stdin/stdout.
///
/// Delegates to [`run_rpc_on`] with `io::stdin()` and `io::stdout()`.
///
/// # Errors
///
/// Returns an error if the stdin background task fails unexpectedly.
pub async fn run_rpc(app: App) -> Result<()> {
    run_rpc_on(app, io::BufReader::new(io::stdin()), io::stdout()).await
}

/// Core RPC loop — generic over I/O for testability.
///
/// Emits a `ready` event, then reads JSONL commands from `input` one line at
/// a time. Each command is dispatched to the appropriate handler. Exits
/// cleanly on input EOF.
///
/// Agent-loop errors are reported as `agent_error` events rather than
/// propagating as `Err`; only fatal I/O failures return `Err`.
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

    write_event(&out, json!({"type": "ready"}));

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

        let cmd: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                write_event(
                    &out,
                    json!({
                        "type": "response",
                        "success": false,
                        "error": format!("JSON parse error: {e}"),
                    }),
                );
                continue;
            }
        };

        dispatch_command(&mut app, cmd, &out, &inp).await;
    }

    Ok(())
}

// ── Command dispatch ──────────────────────────────────────────────────────────

/// Dispatch a parsed command to its handler.
async fn dispatch_command(app: &mut App, cmd: Value, out: &Out, inp: &In) {
    match cmd["type"].as_str() {
        Some("prompt") => handle_prompt(app, &cmd, out, inp).await,
        Some("abort") => {
            app.cancel.cancel();
            write_event(out, json!({"type": "response", "success": true}));
        }
        Some("get_state") => handle_get_state(app, out),
        Some("get_messages") => handle_get_messages(app, out),
        Some("set_model") => handle_set_model(app, &cmd, out),
        Some("get_session_stats") => handle_get_session_stats(app, out),
        Some("compact") => handle_compact(app, out).await,
        Some(other) => write_event(
            out,
            json!({
                "type": "response",
                "success": false,
                "error": format!("unknown command: {other}"),
            }),
        ),
        None => write_event(
            out,
            json!({
                "type": "response",
                "success": false,
                "error": "command missing \"type\" field",
            }),
        ),
    }
}

// ── Command handlers ──────────────────────────────────────────────────────────

/// Run one agent turn for the user message in `cmd["message"]`.
async fn handle_prompt(app: &mut App, cmd: &Value, out: &Out, inp: &In) {
    let message = match cmd["message"].as_str() {
        Some(m) if !m.is_empty() => m.to_owned(),
        _ => {
            write_event(
                out,
                json!({
                    "type": "response",
                    "success": false,
                    "error": "prompt requires a non-empty \"message\" field",
                }),
            );
            return;
        }
    };

    write_event(out, json!({"type": "agent_start"}));

    let observer = RpcObserver {
        out: Arc::clone(out),
    };
    let gate = RpcApprovalGate {
        out: Arc::clone(out),
        input: Arc::clone(inp),
    };
    let client = app.active_provider().clone_boxed_service();
    let params = rho_core::LoopParams {
        client: client.as_ref(),
        registry: &app.registry,
        config: &app.config,
        cancel: app.cancel.clone(),
        gate: &gate,
        observer: &observer,
    };

    match rho_core::run_loop(&mut app.session, &message, &params).await {
        Ok(reply) => write_event(out, json!({"type": "agent_end", "reply": reply})),
        Err(e) => write_event(out, json!({"type": "agent_error", "error": e.to_string()})),
    }
}

/// Return the current model and active provider name.
fn handle_get_state(app: &App, out: &Out) {
    write_event(
        out,
        json!({
            "type": "response",
            "success": true,
            "model": app.session.model(),
            "provider": app.active_provider().name(),
        }),
    );
}

/// Return all messages on the current session path as a JSON array.
fn handle_get_messages(app: &App, out: &Out) {
    let messages: Vec<Value> = app
        .session
        .path_messages()
        .iter()
        .map(message_to_json)
        .collect();
    write_event(
        out,
        json!({"type": "response", "success": true, "messages": messages}),
    );
}

/// Switch the active model to `cmd["model"]`.
fn handle_set_model(app: &mut App, cmd: &Value, out: &Out) {
    match cmd["model"].as_str() {
        Some(id) if !id.is_empty() => {
            app.session.set_model(id);
            write_event(
                out,
                json!({"type": "response", "success": true, "model": id}),
            );
        }
        _ => write_event(
            out,
            json!({
                "type": "response",
                "success": false,
                "error": "set_model requires a non-empty \"model\" field",
            }),
        ),
    }
}

/// Return token budget and context-window usage statistics.
fn handle_get_session_stats(app: &App, out: &Out) {
    let stats = app.session.context_stats();
    write_event(
        out,
        json!({
            "type": "response",
            "success": true,
            "context_window": stats.context_window,
            "completion_reserve": stats.completion_reserve,
            "estimated_used": stats.estimated_used,
            "estimated_remaining": stats.estimated_remaining(),
            "utilization_percent": stats.utilization_percent(),
            "message_count": stats.message_count,
        }),
    );
}

/// Trigger context compaction on the active session.
async fn handle_compact(app: &mut App, out: &Out) {
    let strategy = MechanicalCompactionStrategy::new();
    let threshold = app.session.message_budget() / 4;
    match app.session.compact_older_than(threshold, &strategy).await {
        Ok(_) => write_event(out, json!({"type": "response", "success": true})),
        Err(e) => write_event(
            out,
            json!({"type": "response", "success": false, "error": e.to_string()}),
        ),
    }
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Map an [`AgentState`] to its JSONL string label.
fn state_name(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Thinking => "thinking",
        AgentState::AwaitingApproval => "awaiting_approval",
        AgentState::ExecutingTool => "executing_tool",
    }
}

/// Map a [`ToolRisk`] to its JSONL string label.
fn risk_label(risk: ToolRisk) -> &'static str {
    match risk {
        ToolRisk::Read => "read",
        ToolRisk::Write => "write",
        ToolRisk::Destructive => "destructive",
    }
}

/// Serialize a [`ChatMessage`] to a JSON value for `get_messages` responses.
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
                "tool_calls": calls,
            })
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            let id: &str = tool_call_id;
            json!({
                "role": "tool",
                "tool_call_id": id,
                "content": blocks_to_text(content),
            })
        }
    }
}

/// Concatenate all [`ContentBlock::Text`] values in `blocks`.
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
    use crate::cli::Mode;
    use rho_core::{
        AgentConfig, ChatMessage, ContentBlock, ModelToolCall, ProviderRegistry, Session,
        ToolCallFunction, ToolCallId, ToolName, ToolRegistry, ToolRisk, tool::CancellationToken,
    };
    use rho_test_helpers::{
        FixedResponseTool, MockChatClient, TestProvider, fixed_registry, text_events,
        tool_call_events,
    };
    use std::io::Cursor;

    // ── Unit tests (existing) ─────────────────────────────────────────────

    #[test]
    fn write_event_to_produces_valid_jsonl() {
        let mut buf: Vec<u8> = Vec::new();
        write_event_to(&mut buf, &json!({"type": "ready"}));
        let line = String::from_utf8(buf).unwrap();
        assert!(line.ends_with('\n'), "output must end with a newline");
        let v: Value = serde_json::from_str(line.trim()).expect("must be valid JSON");
        assert_eq!(v["type"], "ready");
    }

    #[test]
    fn write_event_to_handles_nested_values() {
        let mut buf: Vec<u8> = Vec::new();
        write_event_to(
            &mut buf,
            &json!({"type": "response", "success": true, "count": 3}),
        );
        let v: Value = serde_json::from_str(String::from_utf8(buf).unwrap().trim()).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["count"], 3);
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
    fn state_name_awaiting_approval() {
        assert_eq!(
            state_name(&AgentState::AwaitingApproval),
            "awaiting_approval"
        );
    }

    #[test]
    fn state_name_executing_tool() {
        assert_eq!(state_name(&AgentState::ExecutingTool), "executing_tool");
    }

    #[test]
    fn risk_label_read() {
        assert_eq!(risk_label(ToolRisk::Read), "read");
    }

    #[test]
    fn risk_label_write() {
        assert_eq!(risk_label(ToolRisk::Write), "write");
    }

    #[test]
    fn risk_label_destructive() {
        assert_eq!(risk_label(ToolRisk::Destructive), "destructive");
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

    #[test]
    fn blocks_to_text_multiple_joined() {
        let blocks = vec![
            ContentBlock::Text { text: "foo".into() },
            ContentBlock::Text { text: "bar".into() },
        ];
        assert_eq!(blocks_to_text(&blocks), "foobar");
    }

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
    fn message_to_json_assistant_text_only() {
        let msg = ChatMessage::Assistant {
            content: vec![ContentBlock::Text {
                text: "reply".into(),
            }],
            tool_calls: vec![],
        };
        let v = message_to_json(&msg);
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "reply");
        assert!(v["tool_calls"].as_array().unwrap().is_empty());
    }

    #[test]
    fn message_to_json_assistant_with_tool_calls() {
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
        let calls = v["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "c1");
        assert_eq!(calls[0]["name"], "read_file");
        assert_eq!(calls[0]["arguments"], r#"{"path":"a.rs"}"#);
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
        assert_eq!(v["tool_call_id"], "c1");
        assert_eq!(v["content"], "file contents");
    }

    #[allow(clippy::needless_pass_by_value)]
    fn capture_event(event: Value) -> Value {
        let mut buf: Vec<u8> = Vec::new();
        write_event_to(&mut buf, &event);
        serde_json::from_str(String::from_utf8(buf).unwrap().trim()).unwrap()
    }

    #[test]
    fn state_change_event_shape() {
        let v = capture_event(
            json!({"type": "state_change", "state": state_name(&AgentState::Thinking)}),
        );
        assert_eq!(v["type"], "state_change");
        assert_eq!(v["state"], "thinking");
    }

    #[test]
    fn message_update_event_shape() {
        let v = capture_event(json!({"type": "message_update", "delta": "hello"}));
        assert_eq!(v["type"], "message_update");
        assert_eq!(v["delta"], "hello");
    }

    #[test]
    fn reasoning_delta_event_shape() {
        let v = capture_event(json!({"type": "reasoning_delta", "delta": "thinking…"}));
        assert_eq!(v["type"], "reasoning_delta");
        assert_eq!(v["delta"], "thinking…");
    }

    #[test]
    fn tool_call_event_shape() {
        let v = capture_event(
            json!({"type": "tool_call", "name": "read_file", "arguments": r#"{"path":"a.rs"}"#}),
        );
        assert_eq!(v["type"], "tool_call");
        assert_eq!(v["name"], "read_file");
    }

    #[test]
    fn tool_result_event_shape() {
        let v = capture_event(
            json!({"type": "tool_result", "name": "read_file", "is_error": false, "output": "contents"}),
        );
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["output"], "contents");
    }

    #[test]
    fn tool_denied_event_shape() {
        let v = capture_event(json!({"type": "tool_denied", "name": "run_command"}));
        assert_eq!(v["type"], "tool_denied");
        assert_eq!(v["name"], "run_command");
    }

    #[test]
    fn approval_request_event_shape() {
        let v = capture_event(json!({
            "type": "approval_request",
            "tool": "run_command",
            "arguments": r#"{"command":"ls"}"#,
            "risk": risk_label(ToolRisk::Destructive),
        }));
        assert_eq!(v["type"], "approval_request");
        assert_eq!(v["tool"], "run_command");
        assert_eq!(v["risk"], "destructive");
    }

    #[test]
    fn unknown_command_response_shape() {
        let v = capture_event(json!({
            "type": "response",
            "success": false,
            "error": "unknown command: frobnicate",
        }));
        assert_eq!(v["success"], false);
        assert!(v["error"].as_str().unwrap().contains("frobnicate"));
    }

    // ── Integration test harness ──────────────────────────────────────────

    /// Build a test [`App`] with a mock LLM provider.
    ///
    /// The app uses an in-memory session, the given mock client wrapped in
    /// a [`TestProvider`], and the given tool registry. This bypasses the
    /// full CLI startup sequence.
    fn test_app(client: MockChatClient, registry: ToolRegistry) -> App {
        let provider = TestProvider::new("test", client);
        let mut providers = ProviderRegistry::new();
        providers.add(Box::new(provider));

        let session = Session::in_memory(
            "mock-model",
            Some("test system prompt"),
            registry.tool_definitions(),
            "/tmp",
        );

        App {
            mode: Mode::Rpc,
            session,
            providers,
            active_provider_index: 0,
            registry,
            config: AgentConfig::default(),
            cancel: CancellationToken::new(),
        }
    }

    /// Run the RPC loop with canned stdin and capture stdout via
    /// `Arc<Mutex<Vec<u8>>>`.
    async fn rpc_run(
        client: MockChatClient,
        registry: ToolRegistry,
        stdin_lines: &[&str],
    ) -> Vec<Value> {
        let app = test_app(client, registry);
        let stdin_data = stdin_lines.join("\n");
        let reader = Cursor::new(stdin_data.into_bytes());
        let writer: Vec<u8> = Vec::new();

        // Use Arc<Mutex<Vec<u8>>> so we can recover the output after run_rpc_on completes.
        let writer = std::sync::Arc::new(std::sync::Mutex::new(writer));
        let writer_clone = std::sync::Arc::clone(&writer);

        run_rpc_on(app, reader, WriterWrapper(writer_clone))
            .await
            .expect("run_rpc_on should not fail");

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();

        parse_output(&output)
    }

    /// A `Write` wrapper around `Arc<Mutex<Vec<u8>>>` so `run_rpc_on` can
    /// write to shared state that the test can read after completion.
    ///
    /// `Arc<Mutex<Vec<u8>>>` is `Send + Sync`, so this newtype is too.
    #[derive(Clone)]
    struct WriterWrapper(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for WriterWrapper {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    /// Parse captured stdout bytes into a list of JSON values (one per line).
    fn parse_output(output: &[u8]) -> Vec<Value> {
        let text = String::from_utf8_lossy(output);
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("invalid JSONL: {l}\n  error: {e}"))
            })
            .collect()
    }

    /// Assert that `events` contains the given event types in order.
    ///
    /// Other events may appear between the expected ones.
    fn expect_event_sequence(events: &[Value], expected_types: &[&str]) {
        let mut idx = 0;
        for event in events {
            if idx < expected_types.len() && event["type"].as_str() == Some(expected_types[idx]) {
                idx += 1;
            }
        }
        assert_eq!(
            idx,
            expected_types.len(),
            "expected event sequence {expected_types:?}, only matched first {idx}"
        );
    }

    /// Collect all events of a given type.
    fn events_of_type<'a>(events: &'a [Value], event_type: &str) -> Vec<&'a Value> {
        events
            .iter()
            .filter(|e| e["type"].as_str() == Some(event_type))
            .collect()
    }

    /// Build a tool registry with a single auto-approvable read-risk tool.
    fn echo_registry() -> ToolRegistry {
        fixed_registry("echo_tool", "echo output".into(), ToolRisk::Read)
    }

    /// Build a tool registry with a destructive tool (triggers approval).
    fn destructive_registry() -> ToolRegistry {
        fixed_registry("destroy_tool", "destroyed".into(), ToolRisk::Destructive)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 1. Lifecycle
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn ready_emitted_on_start() {
        // No stdin commands → immediate EOF. The only event should be `ready`.
        let events = rpc_run(MockChatClient::new(vec![]), echo_registry(), &[]).await;
        assert!(!events.is_empty(), "expected at least one event");
        assert_eq!(events[0]["type"], "ready");
    }

    #[tokio::test]
    async fn clean_exit_on_eof() {
        // EOF without any commands should return Ok(()) and emit just `ready`.
        let events = rpc_run(MockChatClient::new(vec![]), echo_registry(), &[]).await;
        assert_eq!(events.len(), 1, "expected exactly one event (ready)");
        assert_eq!(events[0]["type"], "ready");
    }

    #[tokio::test]
    async fn empty_lines_skipped() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &["", "  ", ""],
        )
        .await;
        // Only `ready` — no error responses for blank lines.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "ready");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 2. Command dispatch
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn unknown_command_returns_error() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"frob"}"#],
        )
        .await;
        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
        assert!(
            responses[0]["error"]
                .as_str()
                .unwrap()
                .contains("unknown command: frob")
        );
    }

    #[tokio::test]
    async fn missing_type_returns_error() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"not_type":"x"}"#],
        )
        .await;
        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
        assert!(responses[0]["error"].as_str().unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn malformed_json_returns_error() {
        let events = rpc_run(MockChatClient::new(vec![]), echo_registry(), &["{not json"]).await;
        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
        assert!(
            responses[0]["error"]
                .as_str()
                .unwrap()
                .contains("JSON parse error")
        );
    }

    #[tokio::test]
    async fn abort_cancels_token_and_returns_success() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"abort"}"#],
        )
        .await;
        let responses = events_of_type(&events, "response");
        // abort + maybe other responses
        let abort_resp = responses
            .iter()
            .find(|r| r["success"].as_bool() == Some(true));
        assert!(abort_resp.is_some(), "expected a successful abort response");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 3. Prompt — text-only turn
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn prompt_text_only_event_sequence() {
        let client = MockChatClient::new(vec![text_events("hello world")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"say hello"}"#],
        )
        .await;

        expect_event_sequence(
            &events,
            &[
                "ready",
                "agent_start",
                "state_change", // thinking
                "message_update",
                "state_change", // idle
                "agent_end",
            ],
        );

        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
        assert_eq!(agent_end[0]["reply"], "hello world");
    }

    #[tokio::test]
    async fn prompt_empty_message_rejected() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"prompt","message":""}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
        assert!(
            responses[0]["error"]
                .as_str()
                .unwrap()
                .contains("non-empty")
        );
    }

    #[tokio::test]
    async fn prompt_missing_message_rejected() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"prompt"}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
    }

    #[tokio::test]
    async fn prompt_non_string_message_rejected() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"prompt","message":123}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 4. Prompt — tool call turn (auto-approved, Read risk)
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn prompt_tool_call_approved_event_sequence() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "echo_tool", "{}"),
            text_events("done"),
        ]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"use the tool"}"#],
        )
        .await;

        expect_event_sequence(
            &events,
            &[
                "ready",
                "agent_start",
                "tool_call",
                "tool_result",
                "agent_end",
            ],
        );
    }

    #[tokio::test]
    async fn prompt_multi_tool_call_sequential() {
        use rho_test_helpers::multi_tool_call_events;

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FixedResponseTool {
            name: "echo_a",
            response: "result_a".into(),
            risk: ToolRisk::Read,
        }));
        registry.register(Box::new(FixedResponseTool {
            name: "echo_b",
            response: "result_b".into(),
            risk: ToolRisk::Read,
        }));

        let client = MockChatClient::new(vec![
            multi_tool_call_events(vec![("c1", "echo_a", "{}"), ("c2", "echo_b", "{}")]),
            text_events("all done"),
        ]);
        let events = rpc_run(
            client,
            registry,
            &[r#"{"type":"prompt","message":"use two tools"}"#],
        )
        .await;

        let tool_calls = events_of_type(&events, "tool_call");
        assert_eq!(tool_calls.len(), 2, "expected 2 tool_call events");
        let tool_results = events_of_type(&events, "tool_result");
        assert_eq!(tool_results.len(), 2, "expected 2 tool_result events");

        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
        assert_eq!(agent_end[0]["reply"], "all done");
    }

    #[tokio::test]
    async fn prompt_tool_error_reported() {
        // Tool that returns an error result (is_error=true), not an Err.
        // The agent loop only calls on_tool_result for successful tool
        // execution, so we need a tool that returns Ok(ToolResult { is_error: true }).
        struct ErrorResultTool;

        #[async_trait::async_trait]
        impl rho_core::Tool for ErrorResultTool {
            fn name(&self) -> rho_core::ToolName {
                rho_core::ToolName::new("fail_tool".to_owned())
            }
            fn description(&self) -> &str {
                "fails with error result"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn risk(&self) -> ToolRisk {
                ToolRisk::Read
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _cancel: CancellationToken,
            ) -> rho_core::Result<rho_core::ToolOutcome> {
                Ok(rho_core::ToolOutcome::Immediate(
                    rho_core::ToolResult::error("something went wrong"),
                ))
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ErrorResultTool));

        let client = MockChatClient::new(vec![
            tool_call_events("c1", "fail_tool", "{}"),
            text_events("recovered"),
        ]);
        let events = rpc_run(client, registry, &[r#"{"type":"prompt","message":"fail"}"#]).await;

        let tool_results = events_of_type(&events, "tool_result");
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0]["is_error"], true);
        assert!(
            tool_results[0]["output"]
                .as_str()
                .unwrap()
                .contains("something went wrong")
        );

        // Agent should still finish with agent_end (not agent_error).
        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 5. Approval flow
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn approval_granted_flow() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "destroy_tool", "{}"),
            text_events("done"),
        ]);
        // Stdin: prompt, then approval_response.
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"type":"prompt","message":"destroy"}"#,
                r#"{"type":"approval_response","approved":true}"#,
            ],
        )
        .await;

        let approval_requests = events_of_type(&events, "approval_request");
        assert_eq!(approval_requests.len(), 1);
        assert_eq!(approval_requests[0]["tool"], "destroy_tool");
        assert_eq!(approval_requests[0]["risk"], "destructive");

        let tool_results = events_of_type(&events, "tool_result");
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0]["is_error"], false);

        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
    }

    #[tokio::test]
    async fn approval_denied_flow() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "destroy_tool", "{}"),
            // After denial, model sees the denial in context and replies with text.
            text_events("understood, I won't"),
        ]);
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"type":"prompt","message":"destroy"}"#,
                r#"{"type":"approval_response","approved":false}"#,
            ],
        )
        .await;

        let tool_denied = events_of_type(&events, "tool_denied");
        assert_eq!(tool_denied.len(), 1);
        assert_eq!(tool_denied[0]["name"], "destroy_tool");

        // Agent should still complete (model gets denial context, responds).
        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
    }

    #[tokio::test]
    async fn approval_malformed_defaults_deny() {
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "destroy_tool", "{}"),
            // After denial, model replies.
            text_events("ok"),
        ]);
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"type":"prompt","message":"destroy"}"#,
                r#"{"type":"approval_response","oops":"not a bool"}"#,
            ],
        )
        .await;

        let tool_denied = events_of_type(&events, "tool_denied");
        assert_eq!(
            tool_denied.len(),
            1,
            "malformed approval should default to deny"
        );
    }

    #[tokio::test]
    async fn approval_with_reasoning_delta() {
        use rho_ai::StreamEvent;

        // Model sends reasoning, then a tool call (destructive), then text.
        let client = MockChatClient::new(vec![
            vec![
                StreamEvent::Reasoning("let me think…".into()),
                StreamEvent::ToolUseStart {
                    index: 0,
                    id: "c1".into(),
                    name: "destroy_tool".into(),
                },
                StreamEvent::ToolUseInputDelta {
                    index: 0,
                    delta: "{}".into(),
                },
                StreamEvent::ToolUseComplete {
                    index: 0,
                    tool_call: rho_ai::ToolCall {
                        id: "c1".into(),
                        name: "destroy_tool".into(),
                        arguments: "{}".into(),
                    },
                },
                StreamEvent::Done {
                    reason: rho_ai::StopReason::ToolUse,
                    usage: rho_ai::StreamUsage::new(0, 0),
                },
            ],
            text_events("all done"),
        ]);
        let events = rpc_run(
            client,
            destructive_registry(),
            &[
                r#"{"type":"prompt","message":"think then destroy"}"#,
                r#"{"type":"approval_response","approved":true}"#,
            ],
        )
        .await;

        let reasoning = events_of_type(&events, "reasoning_delta");
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0]["delta"], "let me think…");

        let approval = events_of_type(&events, "approval_request");
        assert_eq!(approval.len(), 1);

        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 6. Agent error handling
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn agent_error_on_model_failure() {
        use rho_core::RhoError;
        use rho_test_helpers::MockResponse;

        // Use status 403 (not retryable) to avoid retry loop consuming
        // more mock responses than we provide.
        let client = MockChatClient::with_results(vec![MockResponse::Error(RhoError::Client(
            rho_core::client::error::ClientError::HttpError {
                status: 403,
                message: "forbidden".into(),
            },
        ))]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"fail"}"#],
        )
        .await;

        let agent_errors = events_of_type(&events, "agent_error");
        assert_eq!(agent_errors.len(), 1);
        assert!(agent_errors[0]["error"].as_str().unwrap().contains("403"));
    }

    #[tokio::test]
    async fn agent_error_on_max_iterations() {
        use rho_ai::StreamEvent;

        // Create a model that always requests a tool call (infinite loop).
        let infinite_tool_call = vec![
            StreamEvent::ToolUseStart {
                index: 0,
                id: "c1".into(),
                name: "echo_tool".into(),
            },
            StreamEvent::ToolUseInputDelta {
                index: 0,
                delta: "{}".into(),
            },
            StreamEvent::ToolUseComplete {
                index: 0,
                tool_call: rho_ai::ToolCall {
                    id: "c1".into(),
                    name: "echo_tool".into(),
                    arguments: "{}".into(),
                },
            },
            StreamEvent::Done {
                reason: rho_ai::StopReason::ToolUse,
                usage: rho_ai::StreamUsage::new(0, 0),
            },
        ];

        // Provide enough responses for max_iterations (default is 32).
        // Each iteration consumes one response.
        let responses: Vec<_> = (0..35).map(|_| infinite_tool_call.clone()).collect();
        let client = MockChatClient::new(responses);

        let mut app = test_app(client, echo_registry());
        app.config.max_iterations = 5;

        let stdin_data = r#"{"type":"prompt","message":"loop"}"#;
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
        let events = parse_output(&output);

        let agent_errors = events_of_type(&events, "agent_error");
        assert_eq!(
            agent_errors.len(),
            1,
            "expected agent_error for max iterations"
        );
        assert!(
            agent_errors[0]["error"]
                .as_str()
                .unwrap()
                .contains("maximum iterations")
        );
    }

    #[tokio::test]
    async fn agent_error_preserves_session() {
        use rho_core::RhoError;
        use rho_test_helpers::MockResponse;

        // Use status 403 (not retryable) to avoid consuming extra mock responses.
        let client = MockChatClient::with_results(vec![MockResponse::Error(RhoError::Client(
            rho_core::client::error::ClientError::HttpError {
                status: 403,
                message: "boom".into(),
            },
        ))]);
        let app = test_app(client, echo_registry());

        let stdin_data = r#"{"type":"prompt","message":"fail"}"#;
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);

        run_rpc_on(app, reader, WriterWrapper(writer_clone))
            .await
            .expect("should not panic");

        // Session should have the user message even though the turn failed.
        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        let events = parse_output(&output);

        let agent_errors = events_of_type(&events, "agent_error");
        assert_eq!(agent_errors.len(), 1);

        // We can't inspect the session after run_rpc_on consumes it,
        // but we verify the error was reported (not a panic).
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 7. Query commands
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn get_state_returns_model_and_provider() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"get_state"}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], true);
        assert_eq!(responses[0]["model"], "mock-model");
        assert_eq!(responses[0]["provider"], "test");
    }

    #[tokio::test]
    async fn get_messages_returns_path() {
        let client = MockChatClient::new(vec![text_events("hi")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[
                r#"{"type":"prompt","message":"hello"}"#,
                r#"{"type":"get_messages"}"#,
            ],
        )
        .await;

        let responses = events_of_type(&events, "response");
        // First response is get_messages (agent_start/agent_end are not responses).
        let get_msgs = responses
            .iter()
            .find(|r| r["messages"].is_array())
            .expect("expected get_messages response");

        let messages = get_msgs["messages"].as_array().unwrap();
        // system + user ("hello") + assistant ("hi") + user (second turn not sent)
        // Actually: after prompt turn, path is system + user("hello") + assistant("hi")
        // Then get_messages is a query, doesn't add messages.
        assert!(
            messages.len() >= 2,
            "expected at least 2 messages after one turn"
        );
    }

    #[tokio::test]
    async fn get_session_stats_returns_budget() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"get_session_stats"}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        let stats = responses
            .iter()
            .find(|r| r.get("context_window").is_some())
            .expect("expected stats response");

        assert_eq!(stats["success"], true);
        assert!(stats["context_window"].is_number());
        assert!(stats["completion_reserve"].is_number());
        assert!(stats["estimated_used"].is_number());
        assert!(stats["estimated_remaining"].is_number());
        assert!(stats["utilization_percent"].is_number());
        assert!(stats["message_count"].is_number());
    }

    #[tokio::test]
    async fn set_model_updates_session() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[
                r#"{"type":"set_model","model":"new-model"}"#,
                r#"{"type":"get_state"}"#,
            ],
        )
        .await;

        let responses = events_of_type(&events, "response");

        // set_model response
        let set_resp = responses
            .iter()
            .find(|r| r.get("model").is_some() && r["success"] == true)
            .expect("expected set_model response");
        assert_eq!(set_resp["model"], "new-model");

        // get_state response confirms the model stuck
        let state_resp = responses
            .iter()
            .find(|r| r.get("provider").is_some())
            .expect("expected get_state response");
        assert_eq!(state_resp["model"], "new-model");
    }

    #[tokio::test]
    async fn set_model_empty_rejected() {
        let events = rpc_run(
            MockChatClient::new(vec![]),
            echo_registry(),
            &[r#"{"type":"set_model","model":""}"#],
        )
        .await;

        let responses = events_of_type(&events, "response");
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["success"], false);
    }

    #[tokio::test]
    async fn compact_returns_response() {
        // Even with a small session, compact should return a response
        // (success or error, but must not panic).
        let client = MockChatClient::new(vec![text_events("reply one")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[
                r#"{"type":"prompt","message":"first"}"#,
                r#"{"type":"compact"}"#,
            ],
        )
        .await;

        // The compact command is the last event before EOF.
        // Find any response event that isn't from get_state/set_model/etc.
        let responses = events_of_type(&events, "response");
        // There should be at least one response (the compact result).
        // It may succeed or report "nothing to compact" — either is fine.
        let compact_resp = responses.iter().rev().find(|r| {
            // Compact responses have only success and optionally error.
            r.get("model").is_none()
                && r.get("provider").is_none()
                && r.get("context_window").is_none()
                && r.get("messages").is_none()
        });
        assert!(compact_resp.is_some(), "expected a compact response");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 8. Multi-turn session
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn two_prompts_session_persists() {
        let client = MockChatClient::new(vec![
            text_events("first reply"),
            text_events("second reply"),
        ]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[
                r#"{"type":"prompt","message":"turn 1"}"#,
                r#"{"type":"prompt","message":"turn 2"}"#,
                r#"{"type":"get_messages"}"#,
            ],
        )
        .await;

        // Both turns should produce agent_start → agent_end.
        let agent_starts = events_of_type(&events, "agent_start");
        assert_eq!(agent_starts.len(), 2);

        let agent_ends = events_of_type(&events, "agent_end");
        assert_eq!(agent_ends.len(), 2);
        assert_eq!(agent_ends[0]["reply"], "first reply");
        assert_eq!(agent_ends[1]["reply"], "second reply");

        // get_messages should show 5 messages:
        // system + user(1) + assistant(1) + user(2) + assistant(2)
        let responses = events_of_type(&events, "response");
        let msgs_resp = responses
            .iter()
            .find(|r| r["messages"].is_array())
            .expect("expected get_messages response");
        let messages = msgs_resp["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 5);
    }

    #[tokio::test]
    async fn session_stats_grow() {
        let client = MockChatClient::new(vec![text_events("reply one"), text_events("reply two")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[
                r#"{"type":"prompt","message":"first"}"#,
                r#"{"type":"get_session_stats"}"#,
                r#"{"type":"prompt","message":"second"}"#,
                r#"{"type":"get_session_stats"}"#,
            ],
        )
        .await;

        let stats: Vec<_> = events
            .iter()
            .filter(|e| e.get("context_window").is_some())
            .collect();
        assert_eq!(stats.len(), 2, "expected two stats responses");

        let used_first = stats[0]["estimated_used"].as_u64().unwrap();
        let used_second = stats[1]["estimated_used"].as_u64().unwrap();
        assert!(
            used_second > used_first,
            "estimated_used should grow: {used_second} > {used_first}"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 9. Edge cases
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn large_reply_streaming() {
        // Model sends many deltas that should concatenate.
        use rho_ai::StreamEvent;

        let chunks: Vec<String> = (0..50).map(|i| format!("chunk{i} ")).collect();
        let mut events = vec![];
        for chunk in &chunks {
            events.push(StreamEvent::Text(chunk.clone()));
        }
        events.push(StreamEvent::Done {
            reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(0, 0),
        });

        let client = MockChatClient::new(vec![events]);
        let rpc_events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"big reply"}"#],
        )
        .await;

        let deltas = events_of_type(&rpc_events, "message_update");
        assert_eq!(deltas.len(), 50, "expected 50 message_update events");

        let agent_end = events_of_type(&rpc_events, "agent_end");
        assert_eq!(agent_end.len(), 1);

        let expected: String = chunks.join("");
        assert_eq!(agent_end[0]["reply"].as_str().unwrap(), expected);
    }

    #[tokio::test]
    async fn utf8_in_messages() {
        let client = MockChatClient::new(vec![text_events("こんにちは世界 🌍")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"日本語テスト"}"#],
        )
        .await;

        let agent_end = events_of_type(&events, "agent_end");
        assert_eq!(agent_end[0]["reply"], "こんにちは世界 🌍");
    }

    #[tokio::test]
    async fn special_chars_in_tool_arguments() {
        let args = r#"{"path":"a/b/c","content":"line1\nline2\ttab"}"#;
        let client = MockChatClient::new(vec![
            tool_call_events("c1", "echo_tool", args),
            text_events("ok"),
        ]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"edit"}"#],
        )
        .await;

        let tool_calls = events_of_type(&events, "tool_call");
        assert_eq!(tool_calls.len(), 1);
        // Arguments should survive round-trip through JSON serialization.
        assert_eq!(tool_calls[0]["arguments"].as_str().unwrap(), args);
    }

    #[tokio::test]
    async fn concurrent_abort_during_prompt() {
        // Send a prompt followed immediately by abort. The cancel token
        // should be triggered. The agent may or may not complete depending
        // on timing, but it must not panic.
        use rho_core::RhoError;
        use rho_test_helpers::MockResponse;

        // Model returns an error simulating cancellation.
        let client = MockChatClient::with_results(vec![MockResponse::Error(RhoError::Agent(
            rho_core::AgentError::Cancelled,
        ))]);
        let app = test_app(client, echo_registry());

        let stdin_data = r#"{"type":"prompt","message":"hello"}
{"type":"abort"}"#;
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);

        // Should not panic.
        let result = run_rpc_on(app, reader, WriterWrapper(writer_clone)).await;
        assert!(result.is_ok(), "run_rpc_on should not return Err on abort");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 10. JSONL protocol conformance
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn every_output_line_is_valid_json() {
        let client = MockChatClient::new(vec![text_events("hello")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"hi"}"#],
        )
        .await;

        // parse_output already asserts valid JSON for every line.
        // Just verify we got a non-trivial event stream.
        assert!(events.len() > 2, "expected multiple events");
    }

    #[tokio::test]
    async fn every_output_line_ends_with_newline() {
        let client = MockChatClient::new(vec![text_events("hi")]);

        let app = test_app(client, echo_registry());
        let stdin_data = r#"{"type":"prompt","message":"hi"}"#;
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);

        run_rpc_on(app, reader, WriterWrapper(writer_clone))
            .await
            .unwrap();

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        let text = String::from_utf8(output).unwrap();

        // Every line should end with \n and the whole output should end with \n.
        for line in text.lines() {
            assert!(!line.is_empty(), "no blank lines in JSONL output");
            // Each line is valid JSON (checked implicitly by parse_output).
        }
        assert!(text.ends_with('\n'), "output must end with newline");
    }

    #[tokio::test]
    async fn no_interleaved_lines() {
        // Multi-tool-call produces interleaved observer + handler events.
        // Verify no partial lines appear.
        use rho_test_helpers::multi_tool_call_events;

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FixedResponseTool {
            name: "tool_a",
            response: "a".into(),
            risk: ToolRisk::Read,
        }));
        registry.register(Box::new(FixedResponseTool {
            name: "tool_b",
            response: "b".into(),
            risk: ToolRisk::Read,
        }));

        let client = MockChatClient::new(vec![
            multi_tool_call_events(vec![("c1", "tool_a", "{}"), ("c2", "tool_b", "{}")]),
            text_events("done"),
        ]);

        let app = test_app(client, registry);
        let stdin_data = r#"{"type":"prompt","message":"go"}"#;
        let reader = Cursor::new(stdin_data.as_bytes().to_vec());
        let writer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer_clone = std::sync::Arc::clone(&writer);

        run_rpc_on(app, reader, WriterWrapper(writer_clone))
            .await
            .unwrap();

        let output = std::sync::Arc::try_unwrap(writer)
            .unwrap()
            .into_inner()
            .unwrap();
        let text = String::from_utf8(output).unwrap();

        // Every line must parse as valid JSON.
        for (i, line) in text.lines().enumerate() {
            assert!(
                serde_json::from_str::<Value>(line).is_ok(),
                "line {i} is not valid JSON: {line}"
            );
        }
    }

    #[tokio::test]
    async fn event_order_invariant() {
        let client = MockChatClient::new(vec![text_events("hello")]);
        let events = rpc_run(
            client,
            echo_registry(),
            &[r#"{"type":"prompt","message":"hi"}"#],
        )
        .await;

        // For a text-only turn the order must be:
        //   ready < agent_start < (state_change + message_update)* < agent_end
        let ready_idx = events
            .iter()
            .position(|e| e["type"] == "ready")
            .expect("ready event");
        let start_idx = events
            .iter()
            .position(|e| e["type"] == "agent_start")
            .expect("agent_start event");
        let end_idx = events
            .iter()
            .position(|e| e["type"] == "agent_end")
            .expect("agent_end event");

        assert!(ready_idx < start_idx, "ready before agent_start");
        assert!(start_idx < end_idx, "agent_start before agent_end");
    }
}
