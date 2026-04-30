//! Integration tests for rho-core.
//!
//! Tests use [`MockChatClient`] from `rho-test-helpers` so no live model server
//! is required.

use rho_core::{
    AgentConfig, ChatMessage, ContentBlock, Conversation, RhoError, ToolCallId, ToolName,
    ToolOutcome, ToolRegistry, ToolResult, ToolRisk,
    agent::run_loop,
    message::{ModelToolCall, ToolCallFunction},
    tool::{CancellationToken, Tool},
};
use rho_test_helpers::{
    AutoApproveGate, MockChatClient, load_fixture, multi_tool_call_response, text_response,
    tool_call_response,
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
    //
    // Phase-1 invariant: every Tool message has a preceding Assistant message
    // with the matching tool_call_id. This holds across all phases; the
    // positional `assistant_idx + 1` assertion below is Phase-1-specific
    // (single tool call per turn) and may need updating in Phase 2 when
    // multiple tool calls produce multiple tool results per turn.
    let requests = client.requests();
    assert_eq!(requests.len(), 2, "expected exactly two requests");

    let second = &requests[1];
    let msgs = &second.messages;

    // Structural invariant: every Tool message is preceded by an Assistant
    // message containing the matching tool_call_id.
    let mut prev_was_assistant_with_call = false;
    let mut prev_call_ids: Vec<&str> = Vec::new();
    for msg in msgs {
        match msg {
            ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                prev_was_assistant_with_call = true;
                prev_call_ids = tool_calls.iter().map(|c| c.id.as_ref()).collect();
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                assert!(
                    prev_was_assistant_with_call,
                    "Tool message with call_id '{tool_call_id}' has no preceding Assistant message with tool_calls"
                );
                assert!(
                    prev_call_ids.contains(&tool_call_id.as_ref()),
                    "Tool message with call_id '{tool_call_id}' does not match any preceding tool_call_id: {prev_call_ids:?}"
                );
            }
            ChatMessage::Assistant { .. } => {
                prev_was_assistant_with_call = false;
                prev_call_ids.clear();
            }
            _ => {}
        }
    }
}

// ── Task 5: multi-tool-call handling ─────────────────────────────────────────

#[tokio::test]
async fn multiple_tool_calls_executed_sequentially() {
    // Model requests two tool calls in one response, then returns text.
    let client = MockChatClient::new(vec![
        multi_tool_call_response(vec![("call_1", "echo_a", "{}"), ("call_2", "echo_b", "{}")]),
        text_response("all done"),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool {
        name: "echo_a",
        response: "result_a",
    }));
    registry.register(Box::new(EchoTool {
        name: "echo_b",
        response: "result_b",
    }));

    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let result = run_loop(
        &mut conv,
        "do two things",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap();

    assert_eq!(result, "all done");

    // The second request must contain both tool results after the assistant message.
    let requests = client.requests();
    assert_eq!(requests.len(), 2, "expected exactly two requests");

    let second = &requests[1];
    let msgs = &second.messages;

    // Both tool results must be present.
    let tool_results: Vec<_> = msgs
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .collect();
    assert_eq!(
        tool_results.len(),
        2,
        "expected 2 tool results, got {}: {tool_results:?}",
        tool_results.len()
    );

    // Verify the content of each tool result.
    let result_a = tool_results.iter().find(|m| {
        if let ChatMessage::Tool { content, .. } = m {
            content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "result_a"))
        } else {
            false
        }
    });
    let result_b = tool_results.iter().find(|m| {
        if let ChatMessage::Tool { content, .. } = m {
            content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "result_b"))
        } else {
            false
        }
    });
    assert!(result_a.is_some(), "result_a not found in tool results");
    assert!(result_b.is_some(), "result_b not found in tool results");
}

#[tokio::test]
async fn multi_tool_call_persistence_invariant() {
    // Structural invariant: every Tool message must be preceded by an
    // Assistant message containing the matching tool_call_id.
    // With multiple tool calls in one response, all tool results must
    // reference IDs from the same assistant message.
    let client = MockChatClient::new(vec![
        multi_tool_call_response(vec![
            ("call_1", "echo_tool", "{}"),
            ("call_2", "echo_tool", "{}"),
        ]),
        text_response("done"),
    ]);

    let registry = echo_registry("echo_tool", "echo");
    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let _ = run_loop(
        &mut conv,
        "do two things",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap();

    let requests = client.requests();
    let second = &requests[1];
    let msgs = &second.messages;

    // Collect all tool_call_ids from the assistant message(s).
    let mut prev_was_assistant_with_call = false;
    let mut prev_call_ids: Vec<&str> = Vec::new();
    for msg in msgs {
        match msg {
            ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                prev_was_assistant_with_call = true;
                prev_call_ids = tool_calls.iter().map(|c| c.id.as_ref()).collect();
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                assert!(
                    prev_was_assistant_with_call,
                    "Tool message with call_id '{tool_call_id}' has no preceding Assistant message with tool_calls"
                );
                assert!(
                    prev_call_ids.contains(&tool_call_id.as_ref()),
                    "Tool message with call_id '{tool_call_id}' does not match any preceding tool_call_id: {prev_call_ids:?}"
                );
            }
            ChatMessage::Assistant { .. } => {
                prev_was_assistant_with_call = false;
                prev_call_ids.clear();
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn mixed_approval_with_multi_tool_call() {
    // Model requests one read (auto-approved) and one write (needs approval, denied).
    // The denied tool gets a denial message; the approved tool gets its result.
    // The model then replies with text.
    use rho_test_helpers::AutoDenyGate;

    let client = MockChatClient::new(vec![
        multi_tool_call_response(vec![
            ("call_1", "read_tool", "{}"),
            ("call_2", "write_tool", "{}"),
        ]),
        text_response("understood"),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool {
        name: "read_tool",
        response: "read_result",
    }));
    registry.register(Box::new(WriteEchoTool));

    let config = AgentConfig::default(); // DefaultApprovalPolicy: Read auto, Write needs approval
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let result = run_loop(
        &mut conv,
        "read then write",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoDenyGate, // deny all approval requests
    )
    .await
    .unwrap();

    assert_eq!(result, "understood");

    // The second request must have both tool results:
    // call_1 (read, auto-approved) → "read_result"
    // call_2 (write, denied) → denial message
    let requests = client.requests();
    let second = &requests[1];
    let tool_msgs: Vec<_> = second
        .messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .collect();
    assert_eq!(
        tool_msgs.len(),
        2,
        "expected 2 tool results, got {}",
        tool_msgs.len()
    );

    // First tool result: read_tool executed (auto-approved despite AutoDenyGate)
    if let ChatMessage::Tool {
        content,
        tool_call_id,
    } = tool_msgs[0]
    {
        assert_eq!(
            &**tool_call_id, "call_1",
            "first tool result should be for call_1"
        );
        let text = match &content[0] {
            ContentBlock::Text { text } => text.clone(),
        };
        assert_eq!(text, "read_result");
    }

    // Second tool result: write_tool denied
    if let ChatMessage::Tool {
        content,
        tool_call_id,
    } = tool_msgs[1]
    {
        assert_eq!(
            &**tool_call_id, "call_2",
            "second tool result should be for call_2"
        );
        let text = match &content[0] {
            ContentBlock::Text { text } => text.clone(),
        };
        assert!(
            text.to_lowercase().contains("denied"),
            "denied tool result should mention denial: {text}"
        );
    }
}

/// A write-risk echo tool for testing approval policy.
struct WriteEchoTool;

#[async_trait::async_trait]
impl Tool for WriteEchoTool {
    fn name(&self) -> ToolName {
        ToolName::from("write_tool")
    }
    fn description(&self) -> &'static str {
        "write echo"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        Ok(ToolOutcome::Immediate(ToolResult::success("written")))
    }
}

#[tokio::test]
async fn all_tool_calls_denied_still_feeds_results_and_resends() {
    // Model requests two write tools; both are denied.
    // Both denial messages are appended, then conversation re-sent.
    // Model replies with text.
    use rho_test_helpers::AutoDenyGate;

    let client = MockChatClient::new(vec![
        multi_tool_call_response(vec![
            ("call_1", "write_tool", "{}"),
            ("call_2", "write_tool", "{}"),
        ]),
        text_response("okay, won't write"),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteEchoTool));

    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    let result = run_loop(
        &mut conv,
        "write two files",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoDenyGate,
    )
    .await
    .unwrap();

    assert_eq!(result, "okay, won't write");

    // Both tool results must be denial messages.
    let requests = client.requests();
    let tool_msgs: Vec<_> = requests[1]
        .messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .collect();
    assert_eq!(tool_msgs.len(), 2);
    for msg in &tool_msgs {
        if let ChatMessage::Tool { content, .. } = msg {
            let text = match &content[0] {
                ContentBlock::Text { text } => text.clone(),
            };
            assert!(
                text.to_lowercase().contains("denied"),
                "expected denial: {text}"
            );
        }
    }
}

#[tokio::test]
async fn cancellation_between_tool_calls_in_batch() {
    // Model requests two tool calls. The first is a slow tool that takes
    // time to execute. We cancel while it's running. The loop should
    // exit with an error after the tool returns (or on the next
    // iteration's cancellation check).
    //
    // We use SlowTool (defined below) which polls the cancellation token
    // and returns early if cancelled. This makes the test deterministic.
    let client = MockChatClient::new(vec![multi_tool_call_response(vec![
        ("call_1", "slow_tool", "{}"),
        ("call_2", "slow_tool", "{}"),
    ])]);

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowTool));

    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    // Cancel after 150ms — the SlowTool runs 20 × 50ms = 1000ms polling loop.
    // The first tool will observe the cancellation and return early.
    // The loop then exits on the next iteration because the token is still set.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel_clone.cancel();
    });

    let err = run_loop(
        &mut conv,
        "do two things",
        &client,
        &registry,
        &config,
        cancel,
        &AutoApproveGate,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, RhoError::Unexpected(_)),
        "expected cancellation error, got: {err}"
    );

    // The first tool result (cancelled) should be in conversation history.
    let msgs = conv.messages();
    let has_cancelled_result = msgs.iter().any(|m| {
        matches!(m, ChatMessage::Tool { content, .. } if content.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.contains("cancelled"))
        ))
    });
    assert!(
        has_cancelled_result,
        "expected cancelled tool result in history"
    );
}

#[tokio::test]
async fn empty_tool_calls_vec_returns_error() {
    // Edge case: model returns finish_reason=tool_calls but with an empty vec.
    // This shouldn't happen in practice, but the loop should handle it.
    let client = MockChatClient::new(vec![multi_tool_call_response(Vec::<(
        String,
        String,
        String,
    )>::new())]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
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

    assert!(
        matches!(err, RhoError::Unexpected(_)),
        "expected Unexpected error for empty tool_calls, got: {err}"
    );
}

#[tokio::test]
async fn iteration_count_includes_multi_tool_call_response() {
    // A single model response with multiple tool calls counts as one iteration.
    // The loop should still terminate when the iteration limit is reached.
    let responses: Vec<_> = (0..10)
        .map(|i| {
            multi_tool_call_response(vec![
                (format!("call_{i}a"), "echo_tool", "{}"),
                (format!("call_{i}b"), "echo_tool", "{}"),
            ])
        })
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
        matches!(err, RhoError::MaxIterationsExceeded(5)),
        "expected MaxIterationsExceeded(5), got: {err}"
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

// ── Agent loop: non-retryable errors ───────────────────────────────────────────

#[tokio::test]
async fn non_retryable_error_propagates_immediately() {
    // The mock panics on empty queue, so queue a single non-retryable error.
    let client =
        MockChatClient::with_results(vec![Err(RhoError::Unexpected(anyhow::anyhow!("boom")))]);
    let registry = ToolRegistry::new();
    let config = AgentConfig {
        retry_budget: 4, // high budget, but it should never be touched
        initial_backoff_ms: 0,
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

    // Non-retryable errors should propagate immediately without burning the budget.
    assert!(
        matches!(err, RhoError::Unexpected(_)),
        "expected Unexpected, got: {err}"
    );
}

// ── Agent loop: retry budget ──────────────────────────────────────────────────

/// Create a retryable HTTP error by connecting to an unreachable port.
///
/// Connection-refused errors have no HTTP status code, which
/// [`RhoError::is_retryable`] classifies as retryable.
async fn retryable_http_error() -> RhoError {
    use rho_core::{ChatClient, LocalChatClient};
    let client = LocalChatClient::with_endpoint("http://127.0.0.1:1/");
    let request = rho_core::ChatRequest {
        model: String::new(),
        messages: vec![],
        tools: vec![],
    };
    client.chat(request).await.unwrap_err()
}

#[tokio::test]
async fn retry_budget_exhausted_on_transient_errors() {
    // Sanity check: connection-refused errors are retryable.
    let err = retryable_http_error().await;
    assert!(
        err.is_retryable(),
        "sanity: connection-refused error must be retryable"
    );

    // Queue 3 retryable errors with a budget of 2 → RetryBudgetExhausted.
    let err1 = retryable_http_error().await;
    let err2 = retryable_http_error().await;
    let err3 = retryable_http_error().await;
    let client = MockChatClient::with_results(vec![Err(err1), Err(err2), Err(err3)]);

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

    assert!(
        matches!(err, RhoError::RetryBudgetExhausted(2)),
        "expected RetryBudgetExhausted(2), got: {err}"
    );
}

#[tokio::test]
async fn retry_succeeds_after_transient_error() {
    // Queue 1 retryable error followed by a success.
    let err = retryable_http_error().await;
    let client = MockChatClient::with_results(vec![Err(err), Ok(text_response("recovered"))]);

    let registry = ToolRegistry::new();
    let config = AgentConfig {
        retry_budget: 4,
        initial_backoff_ms: 0, // no delay in tests
        ..AgentConfig::default()
    };
    let mut conv = Conversation::new("mock", None, vec![]);

    let result = run_loop(
        &mut conv,
        "hello",
        &client,
        &registry,
        &config,
        CancellationToken::new(),
        &AutoApproveGate,
    )
    .await
    .unwrap();

    assert_eq!(
        result, "recovered",
        "expected the successful response after retry"
    );

    // The mock should have been called twice: once (failed), then once (succeeded).
    let requests = client.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected exactly 2 requests (1 failed + 1 retry)"
    );
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

/// Tool that polls the cancellation token and returns late if not cancelled.
struct SlowTool;

#[async_trait::async_trait]
impl Tool for SlowTool {
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
        // Poll the token in a loop. If cancelled, return immediately.
        for _ in 0..20 {
            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(ToolOutcome::Immediate(ToolResult::success("done")))
    }
}

#[tokio::test]
async fn cancellation_checked_at_top_of_loop() {
    let client = MockChatClient::new(vec![tool_call_response("call_1", "slow_tool", "{}")]);

    let cancel = CancellationToken::new();
    cancel.cancel(); // cancel before the loop starts

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowTool));

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

    assert!(
        matches!(err, rho_core::RhoError::Unexpected(_)),
        "expected Unexpected error from early cancellation check, got: {err}"
    );
}

#[tokio::test]
async fn cancellation_propagates_into_running_tool() {
    // The model requests a tool call. The tool starts executing and polls
    // the cancellation token. After a short delay, the token is cancelled.
    // The tool should observe the cancellation and return its error result.
    // The loop then exits on the next iteration because the token is still set.
    let client = MockChatClient::new(vec![tool_call_response("call_1", "slow_tool", "{}")]);

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Cancel the token after 150ms — long enough for the tool to start executing
    // but before its 20 × 50ms = 1000ms polling loop finishes.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel_clone.cancel();
    });

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowTool));

    let config = AgentConfig::default();
    let mut conv = Conversation::new("mock", None, registry.tool_schemas());

    // The loop should exit with a cancellation error. The token is still
    // set when the loop re-enters Thinking after the tool returned.
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

    assert!(
        matches!(err, rho_core::RhoError::Unexpected(_)),
        "expected cancellation error, got: {err}"
    );

    // The key assertion: the tool result (cancelled) was fed back into
    // conversation history before the loop exited. This proves cancellation
    // propagated *into* the running tool, not just at the top-of-loop check.
    let msgs = conv.messages();
    let has_cancelled_tool_result = msgs.iter().any(|m| {
        matches!(m, ChatMessage::Tool { content, .. } if content.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.contains("cancelled"))
        ))
    });
    assert!(
        has_cancelled_tool_result,
        "expected the cancelled tool result in conversation history, got: {msgs:?}"
    );
}

// ── LocalChatClient error handling ──────────────────────────────────────────

#[tokio::test]
async fn local_chat_client_returns_http_error_when_server_unreachable() {
    use rho_core::{ChatClient, ChatRequest, LocalChatClient};
    use std::time::Duration;

    let client = LocalChatClient::with_endpoint("http://10.255.255.1/v1/chat/completions");
    let request = ChatRequest {
        model: "test".to_owned(),
        messages: vec![ChatMessage::user_text("hello")],
        tools: vec![],
    };
    let result = tokio::time::timeout(Duration::from_secs(5), client.chat(request)).await;
    // On machines with proxies/VPNs, the connection may time out rather than
    // refuse. Either way, we expect an error (never a success).
    match result {
        Ok(Ok(_)) => panic!("expected error when server is unreachable"),
        Ok(Err(e)) => {
            assert!(
                matches!(e, rho_core::RhoError::Http(_)),
                "expected Http error variant, got: {e}"
            );
        }
        Err(_) => {
            // Timeout is also acceptable — it proves the client handles
            // unreachable servers without hanging indefinitely.
        }
    }
}

// ── base_prompt wiring ────────────────────────────────────────────────────────

#[test]
fn base_prompt_used_as_default_system_message() {
    let prompt = rho_core::base_prompt();
    let conv = Conversation::new("model", Some(prompt), vec![]);
    assert_eq!(conv.system_prompt(), Some(prompt));
}

#[test]
fn compose_system_prompt_identity_with_no_files() {
    // The trivial composition case: no context files → output equals the base.
    assert_eq!(
        rho_core::compose_system_prompt(rho_core::base_prompt(), &[]),
        rho_core::base_prompt()
    );
}

#[test]
fn base_prompt_sha256_is_pinned() {
    // Pin the SHA-256 so any edit to base.md requires updating this test.
    // This makes prompt changes deliberate rather than silent.
    // Normalize line endings before hashing so the test passes on both
    // Windows (CRLF working tree) and CI (LF checkout).
    let normalized = rho_core::base_prompt().replace('\r', "");
    let hash = rho_core::context_files::sha256_hex(&normalized);
    assert_eq!(
        hash, "c600c6c6c80ac6eb07da80db1677dfec1ad13670b61b117b23fa44c049b003ff",
        "base_prompt() hash changed — update this test to match the new hash"
    );
}

#[test]
fn custom_system_overrides_base_prompt() {
    let conv = Conversation::new("model", Some("custom system"), vec![]);
    assert_eq!(conv.system_prompt(), Some("custom system"));
}
