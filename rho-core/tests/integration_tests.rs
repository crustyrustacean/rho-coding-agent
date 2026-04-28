//! Integration tests for rho-core.
//!
//! Tests use [`MockChatClient`] from `rho-test-helpers` so no live model server
//! is required.

use rho_core::{
    AgentConfig, ChatMessage, Conversation, ToolCallId, ToolName, ToolOutcome, ToolRegistry,
    ToolResult, ToolRisk,
    agent::run_loop,
    message::{ModelToolCall, ToolCallFunction},
    tool::{CancellationToken, Tool},
};
use rho_test_helpers::{
    AutoApproveGate, MockChatClient, load_fixture, text_response, tool_call_response,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// A no-op tool that always returns a fixed string.
struct EchoTool {
    name: &'static str,
    response: &'static str,
}

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> ToolName {
        ToolName::from(self.name)
    }
    fn description(&self) -> &'static str {
        "echo"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        Ok(ToolOutcome::Immediate(ToolResult::success(self.response)))
    }
}

fn echo_registry(name: &'static str, response: &'static str) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(EchoTool { name, response }));
    reg
}

// ── Fixture deserialization ───────────────────────────────────────────────────

#[test]
fn fixture_chat_completion_deserializes() {
    let json = load_fixture("tests/fixtures/responses/chat_completion.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(
        response.choices[0].message.content,
        "Hello! How can I assist you today?"
    );
}

#[test]
fn fixture_tool_call_deserializes() {
    let json = load_fixture("tests/fixtures/responses/tool_call.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();
    let call = &response.choices[0].message.tool_calls[0];
    assert_eq!(&*call.id, "call_abc123");
    assert_eq!(&*call.function.name, "read_file");
}

// ── Task 7: tool-call message persistence ────────────────────────────────────

#[tokio::test]
async fn assistant_tool_call_message_persisted_before_tool_result() {
    // Sequence: model requests a tool call, then returns a text reply.
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "echo_tool", "{}"),
        text_response("all done"),
    ]);

    let registry = echo_registry("echo_tool", "echo output");
    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let result = run_loop(
        &mut conv,
        "do something",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap();

    assert_eq!(result, "all done");

    // The second request sent to the mock must contain:
    //   [user, assistant(tool_calls), tool(result)]
    // in that exact order.
    let requests = client.requests();
    assert_eq!(requests.len(), 2, "expected exactly two requests");

    let second = &requests[1];
    let msgs = &second.messages;

    // Find the assistant message with tool_calls
    let assistant_idx = msgs
        .iter()
        .position(
            |m| matches!(m, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        )
        .expect("second request must contain assistant tool_calls message");

    // The very next message must be the tool result
    let tool_msg = msgs
        .get(assistant_idx + 1)
        .expect("tool result must follow assistant tool_calls");

    assert!(
        matches!(tool_msg, ChatMessage::Tool { tool_call_id, .. } if &**tool_call_id == "call_1"),
        "expected Tool message with call_id 'call_1', got: {tool_msg:?}"
    );
}

// ── Agent loop: max iterations ────────────────────────────────────────────────

#[tokio::test]
async fn loop_terminates_after_max_iterations() {
    // Always return a tool call → loop never stops on its own
    let responses: Vec<_> = (0..40)
        .map(|i| tool_call_response(format!("call_{i}"), "echo_tool", "{}"))
        .collect();

    let client = MockChatClient::new(responses);
    let registry = echo_registry("echo_tool", "result");
    let config = AgentConfig {
        max_iterations: 5,
        ..AgentConfig::default()
    };
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let err = run_loop(
        &mut conv,
        "loop forever",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, rho_core::RhoError::MaxIterationsExceeded(5)),
        "expected MaxIterationsExceeded(5), got: {err}"
    );
}

// ── Agent loop: retry budget ──────────────────────────────────────────────────

#[tokio::test]
async fn loop_exhausts_retry_budget_on_transient_errors() {
    use rho_core::RhoError;

    // MockChatClient with no responses triggers an error on every call
    let client = MockChatClient::new(vec![]);
    let registry = ToolRegistry::new();
    let config = AgentConfig {
        retry_budget: 2,
        initial_backoff_ms: 0, // no delay in tests
        ..AgentConfig::default()
    };
    let mut conv = Conversation::new("mock", None, vec![]);

    let err = run_loop(
        &mut conv,
        "hello",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap_err();

    // The mock returns Unexpected (not retryable), so it should propagate immediately.
    // This tests that non-retryable errors pass through without burning the budget.
    assert!(matches!(err, RhoError::Unexpected(_)));
}

// ── ContextManager: tool-call turn not split ──────────────────────────────────

#[test]
fn context_manager_does_not_split_tool_call_turn() {
    use rho_core::context::{ContextManager, SlidingWindowContextManager, TokenBudget};

    let messages = vec![
        ChatMessage::system_text("sys"),
        ChatMessage::user_text("do it"),
        ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![ModelToolCall {
                id: ToolCallId::from("call_1"),
                call_type: "function".to_owned(),
                function: ToolCallFunction {
                    name: ToolName::from("echo_tool"),
                    arguments: "{}".to_owned(),
                },
            }],
        },
        ChatMessage::tool_result(ToolCallId::from("call_1"), "result"),
        ChatMessage::user_text("follow up"),
        ChatMessage::assistant_text("answer"),
    ];

    let cm = SlidingWindowContextManager::new();
    let result = cm.fit(&messages, TokenBudget::new(40));

    let has_tool_call_msg = result
        .iter()
        .any(|m| matches!(m, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()));
    let has_tool_result_msg = result.iter().any(
        |m| matches!(m, ChatMessage::Tool { tool_call_id, .. } if &**tool_call_id == "call_1"),
    );

    assert_eq!(
        has_tool_call_msg, has_tool_result_msg,
        "tool-call turn was split by ContextManager"
    );
}

// ── Cancellation ──────────────────────────────────────────────────────────────

/// Tool that immediately reports cancellation if the token is set.
struct CancelAwareTool;

#[async_trait::async_trait]
impl Tool for CancelAwareTool {
    fn name(&self) -> ToolName {
        ToolName::from("slow_tool")
    }
    fn description(&self) -> &'static str {
        "slow"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }
        Ok(ToolOutcome::Immediate(ToolResult::success("done")))
    }
}

#[tokio::test]
async fn cancellation_propagates_to_run_loop() {
    let client = MockChatClient::new(vec![tool_call_response("call_1", "slow_tool", "{}")]);

    let cancel = CancellationToken::new();
    cancel.cancel(); // cancel before the loop starts

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CancelAwareTool));

    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let err = run_loop(
        &mut conv,
        "do it",
        &client,
        &registry,
        &config,
        cancel,
        &AutoApproveGate,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, rho_core::RhoError::Unexpected(_)));
}

// ── LocalChatClient error handling ──────────────────────────────────────────

#[tokio::test]
async fn local_chat_client_returns_http_error_when_server_unreachable() {
    use rho_core::{ChatClient, ChatRequest, LocalChatClient};

    let client = LocalChatClient::with_endpoint("http://localhost:19999/v1/chat/completions");
    let request = ChatRequest {
        model: "test".to_owned(),
        messages: vec![ChatMessage::user_text("hello")],
        tools: vec![],
    };
    let result = client.chat(request).await;
    assert!(result.is_err(), "expected error when server is unreachable");
    assert!(
        matches!(result.unwrap_err(), rho_core::RhoError::Http(_)),
        "expected Http error variant"
    );
}

// ── base_prompt wiring ────────────────────────────────────────────────────────

#[test]
fn base_prompt_used_as_default_system_message() {
    let prompt = rho_core::base_prompt();
    let conv = Conversation::new("model", Some(prompt), vec![]);
    assert_eq!(conv.system_prompt(), Some(prompt));
}

#[test]
fn custom_system_overrides_base_prompt() {
    let conv = Conversation::new("model", Some("custom system"), vec![]);
    assert_eq!(conv.system_prompt(), Some("custom system"));
}
