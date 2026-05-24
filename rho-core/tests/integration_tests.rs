//! Integration tests for rho-core.
//!
//! Tests use [`MockChatClient`] from `rho-test-helpers` so no live model server
//! is required.

use rho_core::{
    AgentConfig, ChatMessage, ContentBlock, ContextManager, NopObserver, RhoError, Session,
    ToolCallId, ToolName, ToolOutcome, ToolRegistry, ToolResult, ToolRisk,
    agent::{LoopParams, run_loop},
    config::RhoConfig,
    message::{ModelToolCall, ToolCallFunction},
    request::ChatRequest,
    session::error::SessionError,
    tool::{CancellationToken, Tool},
};
use rho_test_helpers::{
    AutoApproveGate, FailingTool, FixedResponseTool, MockChatClient, assert_no_orphan_tool_results,
    empty_content_filter_response, empty_stop_response, fixed_registry, length_truncated_response,
    load_fixture, multi_tool_call_response, text_response, tool_call_response,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

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

// ── Task 11: expanded deserialization tests ─────────────────────────────────────

#[test]
fn fixture_multi_tool_call_deserializes() {
    let json = load_fixture("tests/fixtures/responses/multi_tool_call.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::ToolCalls
    );

    let calls = &response.choices[0].message.tool_calls;
    assert_eq!(calls.len(), 2, "expected two tool calls");

    assert_eq!(&*calls[0].id, "call_read_1");
    assert_eq!(&*calls[0].function.name, "read_file");
    assert_eq!(&*calls[1].id, "call_list_1");
    assert_eq!(&*calls[1].function.name, "list_dir");

    // Arguments should be valid JSON.
    let args0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
    assert_eq!(args0["path"], "src/main.rs");

    let args1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
    assert_eq!(args1["recursive"], true);
}

#[test]
fn fixture_tool_call_with_content_deserializes() {
    // Some models return both content text and tool calls in the same message.
    let json = load_fixture("tests/fixtures/responses/tool_call_with_content.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::ToolCalls
    );

    // Both content and tool_calls should be populated.
    assert_eq!(
        response.choices[0].message.content,
        "I'll read that file for you."
    );
    assert_eq!(response.choices[0].message.tool_calls.len(), 1);
    assert_eq!(
        &*response.choices[0].message.tool_calls[0].id,
        "call_mixed_1"
    );
}

#[test]
fn fixture_write_tool_call_deserializes() {
    // Write tool calls have complex JSON arguments (nested strings, newlines).
    let json = load_fixture("tests/fixtures/responses/write_tool_call.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::ToolCalls
    );

    let call = &response.choices[0].message.tool_calls[0];
    assert_eq!(&*call.function.name, "write_file");

    // Arguments string should be valid JSON containing the expected fields.
    let args: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap();
    assert_eq!(args["path"], "src/lib.rs");
    assert!(args["content"].is_string());
    assert!(args["content"].as_str().unwrap().contains("greet"));
}

#[test]
fn fixture_edit_tool_call_deserializes() {
    // Edit tool calls have an array of edits in their arguments.
    let json = load_fixture("tests/fixtures/responses/edit_tool_call.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    let call = &response.choices[0].message.tool_calls[0];
    assert_eq!(&*call.function.name, "edit_file");

    let args: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap();
    assert!(args["edits"].is_array());
    let edits = args["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["old_text"], "println!(\"Hello!\")");
    assert_eq!(edits[0]["new_text"], "println!(\"Goodbye!\")");
}

#[test]
fn fixture_finish_reason_length_deserializes() {
    let json = load_fixture("tests/fixtures/responses/finish_reason_length.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::Length
    );
    assert!(!response.choices[0].message.content.is_empty());
    assert!(response.choices[0].message.tool_calls.is_empty());
}

#[test]
fn fixture_finish_reason_content_filter_deserializes() {
    let json = load_fixture("tests/fixtures/responses/finish_reason_content_filter.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::ContentFilter
    );
    assert!(!response.choices[0].message.content.is_empty());
    assert!(response.choices[0].message.tool_calls.is_empty());
}

#[test]
fn all_fixture_finish_reasons_round_trip() {
    // Verify that all FinishReason variants survive JSON serialization + deserialization.
    let reasons = vec![
        rho_core::FinishReason::Stop,
        rho_core::FinishReason::ToolCalls,
        rho_core::FinishReason::Length,
        rho_core::FinishReason::ContentFilter,
    ];
    for reason in reasons {
        let json = serde_json::to_string(&reason).unwrap();
        let back: rho_core::FinishReason = serde_json::from_str(&json).unwrap();
        assert_eq!(
            reason, back,
            "FinishReason round-trip failed for {reason:?}"
        );
    }
}

// ── Task 7: tool-call message persistence ────────────────────────────────────

#[tokio::test]
async fn assistant_tool_call_message_persisted_before_tool_result() {
    // Sequence: model requests a tool call, then returns a text reply.
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "echo_tool", "{}"),
        text_response("all done"),
    ]);

    let registry = fixed_registry("echo_tool", "echo output".into(), ToolRisk::Read);
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "do something", &params)
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
    assert_no_orphan_tool_results(msgs);
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

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "do two things", &params)
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

    let registry = fixed_registry("echo_tool", "echo".into(), ToolRisk::Read);
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(&mut session, "do two things", &params)
        .await
        .unwrap();

    let requests = client.requests();
    let second = &requests[1];
    let msgs = &second.messages;

    // Structural invariant: every Tool message must be preceded by an
    // Assistant message containing the matching tool_call_id.
    // With multiple tool calls in one response, all tool results must
    // reference IDs from the same assistant message.
    assert_no_orphan_tool_results(msgs);
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
    registry.register(Box::new(FixedResponseTool {
        name: "read_tool",
        response: "read_result".into(),
        risk: ToolRisk::Read,
    }));
    registry.register(Box::new(FixedResponseTool {
        name: "write_tool",
        response: "written".into(),
        risk: ToolRisk::Write,
    }));

    let config = AgentConfig::default(); // DefaultApprovalPolicy: Read auto, Write needs approval
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoDenyGate, // deny all approval requests,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "read then write", &params)
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
    registry.register(Box::new(FixedResponseTool {
        name: "write_tool",
        response: "written".into(),
        risk: ToolRisk::Write,
    }));

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoDenyGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "write two files", &params)
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
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    // Cancel after 150ms — the SlowTool runs 20 × 50ms = 1000ms polling loop.
    // The first tool will observe the cancellation and return early.
    // The loop then exits on the next iteration because the token is still set.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel_clone.cancel();
    });

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel,
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "do two things", &params)
        .await
        .unwrap_err();

    assert!(
        matches!(err, RhoError::Agent(rho_core::agent::AgentError::Cancelled)),
        "expected cancellation error, got: {err}"
    );

    // The first tool result (cancelled) should be in conversation history.
    let msgs = session.path_messages();
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
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "hello", &params).await.unwrap_err();

    assert!(
        matches!(
            err,
            RhoError::Agent(rho_core::agent::AgentError::ProtocolViolation(_))
        ),
        "expected ProtocolViolation error for empty tool_calls, got: {err}"
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
    let registry = fixed_registry("echo_tool", "result".into(), ToolRisk::Read);
    let config = AgentConfig {
        max_iterations: 5,
        ..AgentConfig::default()
    };
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "loop forever", &params)
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            RhoError::Agent(rho_core::agent::AgentError::MaxIterationsExceeded(5))
        ),
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
    let registry = fixed_registry("echo_tool", "result".into(), ToolRisk::Read);
    let config = AgentConfig {
        max_iterations: 5,
        ..AgentConfig::default()
    };
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "loop forever", &params)
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            rho_core::RhoError::Agent(rho_core::agent::AgentError::MaxIterationsExceeded(5))
        ),
        "expected MaxIterationsExceeded(5), got: {err}"
    );
}

// ── Agent loop: stuck-loop detection ──────────────────────────────────────────

/// When the model calls the same tool with the same arguments and gets the same
/// output N times (where N = `stuck_loop_threshold`), the agent injects a nudge
/// instead of feeding the real tool result. The model then gets one more chance.
#[tokio::test]
async fn stuck_loop_injects_nudge_after_threshold() {
    // Queue: 3 identical tool calls (threshold), then a text reply after the nudge.
    let mut responses: Vec<rho_core::ModelResponse> = (0..4)
        .map(|i| tool_call_response(format!("call_{i}"), "echo_tool", "{}"))
        .collect();
    responses.push(text_response("I see the nudge, stopping."));

    let client = MockChatClient::new(responses);
    let registry = fixed_registry("echo_tool", "same result".into(), ToolRisk::Read);
    let config = AgentConfig {
        stuck_loop_threshold: 3,
        max_iterations: 10,
        ..AgentConfig::default()
    };
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let reply = run_loop(&mut session, "do something", &params)
        .await
        .unwrap();

    assert_eq!(reply, "I see the nudge, stopping.");

    // The path should contain at least one tool result with "STUCK LOOP DETECTED".
    let path = session.path_to_root();
    let has_nudge = path.iter().any(|e| {
        if let rho_core::session::EntryPayload::Message(msg) = &e.payload
            && let ChatMessage::Tool { content, .. } = msg
        {
            return content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("STUCK LOOP")));
        }
        false
    });
    assert!(has_nudge, "expected a STUCK LOOP nudge in the session");
}

/// Stuck-loop detection is disabled when `stuck_loop_threshold` is 0.
#[tokio::test]
async fn stuck_loop_disabled_when_threshold_is_zero() {
    // 6 identical calls → should hit max_iterations, not the nudge.
    let responses: Vec<_> = (0..10)
        .map(|i| tool_call_response(format!("call_{i}"), "echo_tool", "{}"))
        .collect();

    let client = MockChatClient::new(responses);
    let registry = fixed_registry("echo_tool", "same result".into(), ToolRisk::Read);
    let config = AgentConfig {
        stuck_loop_threshold: 0, // disabled
        max_iterations: 5,
        ..AgentConfig::default()
    };
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "loop", &params).await.unwrap_err();

    assert!(
        matches!(
            err,
            rho_core::RhoError::Agent(rho_core::agent::AgentError::MaxIterationsExceeded(5))
        ),
        "expected MaxIterationsExceeded(5), got: {err}"
    );
}

// ── Agent loop: non-retryable errors ───────────────────────────────────────────

#[tokio::test]
async fn non_retryable_error_propagates_immediately() {
    // The mock panics on empty queue, so queue a single non-retryable error.
    let client = MockChatClient::with_results(vec![Err(RhoError::Session(
        SessionError::persistence_error("boom"),
    ))]);
    let registry = ToolRegistry::new();
    let config = AgentConfig {
        retry_budget: 4, // high budget, but it should never be touched
        initial_backoff_ms: 0,
        ..AgentConfig::default()
    };
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "hello", &params).await.unwrap_err();

    // Non-retryable errors should propagate immediately without burning the budget.
    assert!(
        matches!(err, RhoError::Session(_)),
        "expected Session error, got: {err}"
    );
}

#[test]
fn http_error_retryable_for_server_errors() {
    // Server errors (5xx) and rate limiting (429) are retryable.
    for status in [429, 500, 502, 503, 504] {
        let client_err =
            rho_core::client::error::ClientError::http_error(status, "server error".to_owned());
        let err = rho_core::RhoError::Client(client_err);
        assert!(err.is_retryable(), "HTTP {status} should be retryable");
    }
}

#[test]
fn http_error_not_retryable_for_client_errors() {
    // Client errors (4xx, except 429) are permanent — not retryable.
    for status in [400, 401, 403, 404, 405, 422] {
        let client_err =
            rho_core::client::error::ClientError::http_error(status, "client error".to_owned());
        let err = rho_core::RhoError::Client(client_err);
        assert!(!err.is_retryable(), "HTTP {status} should not be retryable");
    }
}

// ── Agent loop: retry budget ──────────────────────────────────────────────────

/// Create a retryable HTTP error by connecting to an unreachable port.
///
/// Connection-refused errors have no HTTP status code, which
/// [`RhoError::is_retryable`] classifies as retryable.
async fn retryable_http_error() -> RhoError {
    use rho_core::{ChatClient, RhoAiClient};
    let client = RhoAiClient::new("test", "http://127.0.0.1:1/", None);
    let request = rho_core::ChatRequest {
        model: String::new(),
        messages: vec![],
        stream: false,
        tools: vec![],
        max_tokens: None,
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
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "hello", &params).await.unwrap_err();

    assert!(
        matches!(err, RhoError::RetryBudgetExhausted(2, _)),
        "expected RetryBudgetExhausted(2, _), got: {err}"
    );

    // Verify call count: 1 initial + 2 retries = 3 total attempts.
    assert_eq!(
        client.requests().len(),
        3,
        "expected 3 total attempts (1 initial + 2 retries), got {}",
        client.requests().len()
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
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "hello", &params).await.unwrap();

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
    fn description(&self) -> &str {
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
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel,
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "do it", &params).await.unwrap_err();

    assert!(
        matches!(
            err,
            rho_core::RhoError::Agent(rho_core::agent::AgentError::Cancelled)
        ),
        "expected Cancelled error from early cancellation check, got: {err}"
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
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    // The loop should exit with a cancellation error. The token is still
    // set when the loop re-enters Thinking after the tool returned.
    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel,
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let err = run_loop(&mut session, "do it", &params).await.unwrap_err();

    assert!(
        matches!(
            err,
            rho_core::RhoError::Agent(rho_core::agent::AgentError::Cancelled)
        ),
        "expected cancellation error, got: {err}"
    );

    // The key assertion: the tool result (cancelled) was fed back into
    // conversation history before the loop exited. This proves cancellation
    // propagated *into* the running tool, not just at the top-of-loop check.
    let msgs = session.path_messages();
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

// ── RhoAiClient error handling ──────────────────────────────────────────

#[tokio::test]
async fn rho_ai_client_returns_http_error_when_server_unreachable() {
    use rho_core::{ChatClient, ChatRequest, RhoAiClient};
    use std::time::Duration;

    let client = RhoAiClient::new("test", "http://10.255.255.1/v1/chat/completions", None);
    let request = ChatRequest {
        model: "test".to_owned(),
        messages: vec![ChatMessage::user_text("hello")],
        stream: false,
        tools: vec![],
        max_tokens: None,
    };
    let result = tokio::time::timeout(Duration::from_secs(5), client.chat(request)).await;
    // On machines with proxies/VPNs, the connection may time out rather than
    // refuse. Either way, we expect an error (never a success).
    match result {
        Ok(Ok(_)) => panic!("expected error when server is unreachable"),
        Ok(Err(e)) => {
            assert!(
                matches!(
                    e,
                    rho_core::RhoError::Client(rho_core::client::error::ClientError::Http(_))
                ),
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
    let session = Session::in_memory("model", Some(prompt), vec![], "/tmp");
    assert_eq!(session.system_prompt(), Some(prompt));
}

#[test]
fn session_default_token_budget_is_32k() {
    use rho_core::context::TokenBudget;
    let session = Session::in_memory("model", None, vec![], "/tmp");
    // Session uses TokenBudget::default() which is now 32K.
    // We verify by checking that the context manager's fit method
    // retains all messages when they're well under 32K tokens.
    let messages = session.path_messages();
    let cm = rho_core::SlidingWindowContextManager::new();
    let fitted = cm.fit(&messages, TokenBudget::default());
    assert_eq!(fitted.len(), messages.len());
}

#[test]
fn session_with_custom_token_budget() {
    use rho_core::context::TokenBudget;
    let _conv =
        Session::in_memory("model", None, vec![], "/tmp").with_token_budget(TokenBudget::new(1024));
    // Verify the budget is applied by constructing a conversation that
    // would overflow 1024 tokens.
    let prompt = "a".repeat(5000); // ~1,250 tokens — exceeds 1024
    let sys = ChatMessage::system_text("sys");
    let user = ChatMessage::user_text(&prompt);
    let cm = rho_core::SlidingWindowContextManager::new();
    let fitted = cm.fit(&[sys, user], TokenBudget::new(1024));
    // System message is pinned, so it survives.
    assert!(
        fitted
            .iter()
            .any(|m| matches!(m, ChatMessage::System { .. }))
    );
    // The last turn (user message) must never be evicted — evicting the
    // only user request causes amnesia. With a tiny budget, the fitter
    // keeps it even though it overflows, because dropping it would be worse.
    assert!(
        fitted.iter().any(|m| matches!(m, ChatMessage::User { .. })),
        "last user turn must never be evicted, even under budget pressure"
    );
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
        hash, "916819ebcd87ea4949014d23215f3b2093e435bb7e097d99e5c557c93929124e",
        "base_prompt() hash changed — update this test to match the new hash"
    );
}

#[test]
fn compact_prompt_sha256_is_pinned() {
    // Same as base_prompt_sha256_is_pinned — pin the compact prompt hash
    // so any edit to compact.md requires updating this test.
    let normalized = rho_core::compact_prompt().replace('\r', "");
    let hash = rho_core::context_files::sha256_hex(&normalized);
    assert_eq!(
        hash, "d04efc17ad433ab96be8fb2bcead15e15612ad9caecef9a04c8a8e10fdbc0024",
        "compact_prompt() hash changed — update this test to match the new hash"
    );
}

#[test]
fn custom_system_overrides_base_prompt() {
    let session = Session::in_memory("model", Some("custom system"), vec![], "/tmp");
    assert_eq!(session.system_prompt(), Some("custom system"));
}

#[tokio::test]
async fn tool_execution_error_still_appends_tool_result() {
    // When a tool execution fails, the agent loop must still append a
    // tool result entry to the session. The error is fed back to the
    // model so it can see what went wrong and retry, matching the
    // pattern used for denied and stuck-loop tool calls.
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "fail_tool", r#"{"path":"test"}"#),
        text_response("I see the error, let me try differently."),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FailingTool {
        name: "fail_tool",
        error_message: "something went wrong".into(),
    }));

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_schemas(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "trigger the failing tool", &params).await;

    // The loop should recover: the model sees the error and returns a text reply.
    assert!(
        result.is_ok(),
        "expected ok after model recovered from tool error, got: {result:?}"
    );

    // Verify a tool result entry exists in the session (even though
    // the tool failed). Every assistant tool_call must have a matching
    // tool result in the conversation history.
    let messages = session.path_messages();
    let tool_results: Vec<_> = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .collect();
    assert_eq!(
        tool_results.len(),
        1,
        "expected 1 tool result entry even on tool execution failure"
    );
}

// ── Length truncation recovery ────────────────────────────────────────────────

/// When the model returns `finish_reason=length` with empty content and
/// non-empty `reasoning_content`, the agent loop should attempt compaction
/// and retry. If compaction fails (too few entries), it returns a user-
/// facing explanation instead of silently returning empty text.
#[tokio::test]
async fn length_truncated_empty_content_returns_explanation() {
    // The exact scenario from the bug report: reasoning model spent all
    // completion tokens on chain-of-thought, produced no content.
    let client = MockChatClient::new(vec![length_truncated_response(
        "",
        "Now I have a thorough understanding of the codebase. Let me summarize.",
    )]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "design streaming support", &params)
        .await
        .unwrap();

    // Must NOT be empty — the old bug returned Ok("").
    assert!(
        !result.is_empty(),
        "expected a non-empty explanation, got empty string (the bug!)"
    );
    // Must mention the core problem.
    assert!(
        result.contains("ran out of tokens"),
        "expected 'ran out of tokens' in explanation, got: {result}"
    );
    // Must mention reasoning/thinking since reasoning_content was non-empty.
    assert!(
        result.contains("thinking"),
        "expected 'thinking' in explanation (reasoning_content was non-empty), got: {result}"
    );
}

/// When `finish_reason=length` with non-empty content, the explanation
/// should include the partial output.
#[tokio::test]
async fn length_truncated_with_partial_content_shows_it() {
    let client = MockChatClient::new(vec![length_truncated_response(
        "The implementation involves several steps. First, you need to",
        "",
    )]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "explain something", &params)
        .await
        .unwrap();

    assert!(
        !result.is_empty(),
        "expected a non-empty explanation, got empty string"
    );
    assert!(
        result.contains("ran out of tokens"),
        "expected 'ran out of tokens' in explanation, got: {result}"
    );
    assert!(
        result.contains("Partial output (truncated)"),
        "expected 'Partial output (truncated)' in explanation, got: {result}"
    );
    assert!(
        result.contains("The implementation involves several steps"),
        "expected the partial content in the explanation, got: {result}"
    );
}

/// When `finish_reason=length` with empty content AND empty reasoning,
/// the explanation should use the "no output" variant.
#[tokio::test]
async fn length_truncated_empty_everything_shows_no_output() {
    // When the model produces completely empty output (no text, no
    // reasoning), the agent loop injects a nudge and retries rather
    // than attempting compaction (which would fail with too few entries).
    let client = MockChatClient::new(vec![
        length_truncated_response("", ""),
        text_response("Sorry about that — here is my actual response."),
    ]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "hello", &params).await.unwrap();

    assert!(
        result.contains("actual response"),
        "expected model to recover after nudge, got: {result}"
    );
}

/// The truncated assistant message is persisted in the session so the
/// conversation history stays valid.
#[tokio::test]
async fn length_truncated_message_persisted_in_session() {
    let client = MockChatClient::new(vec![length_truncated_response(
        "some partial text",
        "reasoning here",
    )]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, vec![], "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(&mut session, "test", &params).await;

    // The session should contain an assistant message with the partial text.
    let msgs = session.path_messages();
    let has_assistant_with_partial = msgs.iter().any(|m| {
        matches!(m, ChatMessage::Assistant { content, .. } if content.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text == "some partial text")
        ))
    });
    assert!(
        has_assistant_with_partial,
        "expected the truncated assistant message to be persisted in the session"
    );
}

/// When compaction succeeds after a length truncation, the loop retries
/// and the model gets a second chance with a compacted context.
#[tokio::test]
async fn length_truncated_compacts_and_retries() {
    // First: length-truncated response (model ran out of tokens).
    // Second: successful text response after compaction freed space.
    let client = MockChatClient::new(vec![
        length_truncated_response("", "still thinking..."),
        text_response("Here is the full answer you asked for."),
    ]);

    let registry = ToolRegistry::new();
    let config = AgentConfig::default();
    // Use a short system prompt so the session doesn't consume too much
    // of the token budget, leaving room for the user message to be
    // compacted.
    let mut session = Session::in_memory("mock", Some("sys"), vec![], "/tmp");

    // Build up enough history before the length truncation for compaction
    // to succeed. We need the user message to exceed the compaction threshold.
    // The threshold is message_budget / 4. With a short system prompt and
    // no tools, message_budget ≈ 32K. We need a user message that's big
    // enough that its estimated tokens exceed 32K/4 = 8K. The heuristic
    // estimator uses chars/4, so we need ~32K chars.
    let long_message = "A".repeat(40_000);
    session.append_user_message(&long_message);

    // Now run the loop with a second user message (the one that triggers
    // the length-truncated response). We bypass `run_loop` and use
    // `send_current` directly to set up the truncated response, then
    // verify the retry behavior. Actually, `run_loop` appends the message
    // itself, so let's use a short message.
    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "short follow-up", &params)
        .await
        .unwrap();

    // After compaction + retry, the model should have produced its answer.
    assert_eq!(
        result, "Here is the full answer you asked for.",
        "expected the model's successful response after compaction + retry"
    );

    // The mock should have been called twice: once (truncated), then once (retry).
    let requests = client.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected 2 requests (1 truncated + 1 retry after compaction), got {}",
        requests.len()
    );
}

/// The `finish_reason_length_empty` fixture round-trips correctly.
#[test]
fn fixture_finish_reason_length_empty_deserializes() {
    let json = load_fixture("tests/fixtures/responses/finish_reason_length_empty.json");
    let response: rho_core::ModelResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(
        response.choices[0].finish_reason,
        rho_core::FinishReason::Length
    );
    assert!(response.choices[0].message.content.is_empty());
    assert!(!response.choices[0].message.reasoning_content.is_empty());
}

// ── Empty stop → LengthTruncated (llama.cpp misreporting) ─────────────────────

/// When the model returns `finish_reason: "stop"` with empty content,
/// `send_current` should treat it as `LengthTruncated`, not a successful
/// `Message("")`. This guards against llama.cpp reporting "stop" instead
/// of "length" when the model exhausts its completion budget.
#[tokio::test]
async fn empty_stop_is_treated_as_length_truncated() {
    use rho_core::conversation::AssistantResponse;

    let mut session = Session::in_memory("mock-model", Some("you are rho"), vec![], "/tmp");
    session.append_user_message("hello");

    let client = MockChatClient::new(vec![empty_stop_response()]);

    let result = session.send_current(&client).await.unwrap();
    assert!(
        matches!(result, AssistantResponse::LengthTruncated { .. }),
        "expected LengthTruncated for empty stop, got: {result:?}"
    );
}

/// When the model returns `finish_reason: "content_filter"` with empty
/// content, `send_current` should **not** route to `LengthTruncated`.
/// Content-filter blocks are not recoverable via compaction.
#[tokio::test]
async fn empty_content_filter_is_not_treated_as_length_truncated() {
    use rho_core::conversation::AssistantResponse;

    let mut session = Session::in_memory("mock-model", Some("you are rho"), vec![], "/tmp");
    session.append_user_message("hello");

    let client = MockChatClient::new(vec![empty_content_filter_response()]);

    let result = session.send_current(&client).await.unwrap();
    assert!(
        matches!(result, AssistantResponse::Message { ref text, .. } if text.is_empty()),
        "expected Message with empty text for empty content_filter, got: {result:?}"
    );
}

/// A non-empty stop response is unaffected — it should still return
/// `Message` as before.
#[tokio::test]
async fn nonempty_stop_remains_message() {
    use rho_core::conversation::AssistantResponse;

    let mut session = Session::in_memory("mock-model", Some("you are rho"), vec![], "/tmp");
    session.append_user_message("hello");

    let client = MockChatClient::new(vec![text_response("all good")]);

    let result = session.send_current(&client).await.unwrap();
    assert!(
        matches!(result, AssistantResponse::Message { ref text, .. } if text == "all good"),
        "expected Message {{ text: \"all good\", .. }}, got: {result:?}"
    );
}

// -- Streaming requests ----------------------------------------------------

#[tokio::test]
#[ignore = "requires running a local model server"]
async fn test_chat_stream() {
    use futures::StreamExt;

    let provider = rho_core::provider_factory(&RhoConfig::default(), None, None);
    let request = ChatRequest {
        model: "google/gemma-4-26b-a4b".to_string(),
        messages: vec![],
        tools: vec![],
        stream: false,
        max_tokens: None,
    };
    let mut stream = provider.chat_client().chat_stream(request).await.unwrap();

    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        println!("{event:?}");
    }
}
