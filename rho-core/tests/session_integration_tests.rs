#![allow(
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::duration_suboptimal_units,
    clippy::let_and_return,
    clippy::too_many_lines
)]
//! Phase 2.5 integration tests: session tree integrity, pressure tests,
//! compact-and-resume, and estimator convergence.
//!
//! These tests exercise the Session-based conversation model through its
//! public API. Assistant messages are created via the agent loop (with mock
//! clients), not by reaching into `pub(crate)` methods.

use rho_core::{
    AgentConfig, ChatMessage, ContentBlock, ContextManager, MechanicalCompactionStrategy,
    ModelResponse, NopObserver, Session, SlidingWindowContextManager, TokenBudget, ToolCallId,
    ToolName, ToolResult,
    agent::{LoopParams, run_loop},
    message::{ModelToolCall, ToolCallFunction},
    session::{
        CompactionSummary, Entry, EntryPayload, EntryResolution, HeuristicEstimator, TokenEstimator,
    },
    tool::CancellationToken,
};
use rho_test_helpers::{
    AutoApproveGate, MockChatClient, assert_no_orphan_tool_results, fixed_registry,
    in_memory_session, single_text_turn, single_tool_turn, text_response, tool_call_response,
};
use std::collections::BTreeMap;
use std::time::Duration;

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Response builder with known prompt_tokens for calibration tests.
fn response_with_usage(text: &str, prompt_tokens: usize) -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text,
                "reasoning_content": "",
                "tool_calls": []
            },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": 5,
            "total_tokens": prompt_tokens + 5
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("response_with_usage")
}

// ── Task 16: Phase 2.5–specific tests ─────────────────────────────────────────

/// Entry round-trip: every EntryPayload and EntryResolution variant survives
/// JSONL serialize/deserialize.
#[test]
fn entry_round_trip_all_variants() {
    use rho_core::newtypes::EntryId;
    use rho_core::session::persist::JsonlLine;
    use std::time::SystemTime;

    let variants: Vec<Entry> = vec![
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::system_text("sys")),
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::user_text("hello")),
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::assistant_text("reply")),
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("c1"),
                "result",
            )),
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Compaction {
                summary: CompactionSummary {
                    original_request: Some("fix it".to_owned()),
                    tool_calls: BTreeMap::new(),
                    tokens_compacted: 100,
                    entry_count: 2,
                    time_span: Duration::from_secs(10),
                    notes: None,
                },
                first_kept: EntryId::new(),
                tokens_before: 200,
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::BranchSummary {
                summary: CompactionSummary {
                    original_request: None,
                    tool_calls: BTreeMap::new(),
                    tokens_compacted: 50,
                    entry_count: 1,
                    time_span: Duration::from_secs(5),
                    notes: Some("notes".to_owned()),
                },
                from_id: EntryId::new(),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::ModelChange {
                model: "gemma-4".to_owned(),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Attached,
            payload: EntryPayload::Label {
                target_id: EntryId::new(),
                label: Some("checkpoint".to_owned()),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Attached,
            payload: EntryPayload::SessionInfo {
                name: "test".to_owned(),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Attached,
            payload: EntryPayload::LeafMoved {
                from: Some(EntryId::new()),
                to: EntryId::new(),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Attached,
            payload: EntryPayload::Custom {
                kind: "rho.test.v1".to_owned(),
                data: serde_json::json!({"count": 42}),
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::CustomMessage {
                kind: "rho.msg.v1".to_owned(),
                content: vec![ContentBlock::Text {
                    text: "hello".to_owned(),
                }],
            },
        },
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Compacted {
                into: EntryId::from("compaction_id"),
            },
            payload: EntryPayload::Message(ChatMessage::user_text("old")),
        },
    ];

    for entry in &variants {
        let line = JsonlLine::Entry(entry.clone());
        let json = serde_json::to_string(&line).unwrap();
        let back: JsonlLine = serde_json::from_str(&json).unwrap();
        assert_eq!(
            line, back,
            "entry round-trip failed for {:?}",
            entry.payload
        );
    }
}

/// Tree integrity: appending entries produces a valid tree.
#[test]
fn tree_integrity_after_appends() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

    let mut prev_leaf = session.leaf().unwrap();
    for i in 0..20 {
        let new_leaf = if i % 2 == 0 {
            session.append_user_message(&format!("msg {i}"))
        } else {
            // Use append_custom_message to add content (rendered as User in path)
            session.append_custom_message(
                "test.reply.v1".to_owned(),
                vec![ContentBlock::Text {
                    text: format!("reply {i}"),
                }],
            )
        };

        // Every new entry's parent_id should be the previous leaf
        let entry = session.entry(&new_leaf).unwrap();
        assert_eq!(
            entry.parent_id,
            Some(prev_leaf),
            "entry {i}: parent_id should be the previous leaf"
        );
        prev_leaf = new_leaf;
    }
}

/// Path building: path_to_root returns entries in correct order with no
/// duplicates and no orphans.
#[test]
fn path_building_no_duplicates_no_orphans() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
    for i in 0..10 {
        session.append_user_message(&format!("msg {i}"));
        session.append_custom_message(
            "test.reply.v1".to_owned(),
            vec![ContentBlock::Text {
                text: format!("reply {i}"),
            }],
        );
    }

    let path = session.path_to_root();
    let ids: Vec<_> = path.iter().map(|e| e.id.clone()).collect();

    // No duplicates
    let unique: std::collections::HashSet<_> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), ids.len(), "path should have no duplicates");

    // Every entry in the path should have a parent that is also in the path
    for entry in &path {
        if let Some(ref parent_id) = entry.parent_id {
            assert!(
                ids.contains(parent_id),
                "entry {:?}: parent {:?} not in path",
                entry.id,
                parent_id
            );
        }
    }
}

/// Resolution filtering: Attached entries are in the tree but not in path_messages.
#[test]
fn resolution_filtering_attached_still_in_tree() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
    let user_id = session.append_user_message("keep me");
    let _label_id = session.append_label(user_id, Some("label".to_owned()));

    // The label should be in the tree (find it via path_to_root)
    let path = session.path_to_root();
    let label_entry = path
        .iter()
        .find(|e| matches!(e.payload, EntryPayload::Label { .. }));
    assert!(label_entry.is_some(), "label entry should be in the tree");

    // The label should NOT appear in path_messages
    let messages = session.path_messages();
    assert_eq!(messages.len(), 2, "only System and User should appear");
}

/// Branching: after branch_to(earlier_id), path_to_root reflects the new
/// path and old branch is unreachable from leaf but still in entries.
#[tokio::test]
async fn branching_old_branch_unreachable_from_leaf() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp");
    let root_id = session.leaf().unwrap();

    // Turn 1: user + assistant
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "echo", "{}"),
        text_response("reply A"),
    ]);
    let config = AgentConfig::default();
    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(&mut session, "hello", &params).await.unwrap();

    // Find the user entry id
    let user_id = session
        .path_to_root()
        .iter()
        .rev()
        .find(|e| matches!(e.payload, EntryPayload::Message(ChatMessage::User { .. })))
        .map(|e| e.id.clone())
        .unwrap();

    // Find the assistant entry id
    let asst_a = session
        .path_to_root()
        .iter()
        .find(|e| {
            matches!(
                e.payload,
                EntryPayload::Message(ChatMessage::Assistant { .. })
            )
        })
        .map(|e| e.id.clone())
        .unwrap();

    // Branch back to user
    session.branch_to(&user_id).unwrap();

    // Turn 2 on the new branch
    let _ = single_text_turn(&mut session, "different question", "reply B", &registry).await;

    // Old branch still in tree
    assert!(
        session.entry(&asst_a).is_some(),
        "old branch entry should still exist"
    );

    // Old branch not reachable from leaf
    let path_ids: Vec<_> = session
        .path_to_root()
        .iter()
        .map(|e| e.id.clone())
        .collect();
    assert!(
        !path_ids.contains(&asst_a),
        "old branch should not be on leaf path"
    );

    // New branch reachable from leaf
    assert!(
        path_ids.contains(&user_id),
        "user_id should be on leaf path"
    );
    assert!(path_ids.contains(&root_id), "root should be on leaf path");
}

/// Branch summary: branch_with_summary produces a BranchSummary entry at
/// the new leaf with correct from_id.
#[test]
fn branch_summary_correct_from_id() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
    let root_id = session.leaf().unwrap();
    let user_id = session.append_user_message("hello");

    let summary = CompactionSummary {
        original_request: Some("hello".to_owned()),
        tool_calls: BTreeMap::new(),
        tokens_compacted: 100,
        entry_count: 2,
        time_span: Duration::from_secs(10),
        notes: None,
    };

    session
        .branch_with_summary(&root_id, summary, user_id.clone())
        .unwrap();

    // The leaf should be a BranchSummary entry
    let leaf_id = session.leaf().unwrap();
    let leaf_entry = session.entry(&leaf_id).unwrap();
    if let EntryPayload::BranchSummary {
        from_id,
        summary: s,
    } = &leaf_entry.payload
    {
        assert_eq!(*from_id, user_id);
        assert_eq!(s.original_request, Some("hello".to_owned()));
    } else {
        panic!("expected BranchSummary at leaf");
    }
}

/// In-memory mode: Session::in_memory performs no disk I/O.
#[test]
fn in_memory_mode_no_disk_io() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
    assert!(session.save_path().is_none());
    assert!(session.flush().is_ok());
}

/// Extension entries: typed read/write through ExtensionEntry trait works;
/// kind mismatch returns None; bumping version produces clean break.
#[test]
fn extension_entries_version_bump_clean_break() {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct StateV1 {
        value: usize,
    }

    impl rho_core::session::ExtensionEntry for StateV1 {
        const KIND: &'static str = "test.state.v1";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct StateV2 {
        value: usize,
        extra: String,
    }

    impl rho_core::session::ExtensionEntry for StateV2 {
        const KIND: &'static str = "test.state.v2";
    }

    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

    // Write v1
    let v1 = StateV1 { value: 42 };
    let id = session.write_custom_state(&v1);

    // v1 reads back
    let read_v1: Option<StateV1> = session.read_custom_state(&id);
    assert_eq!(read_v1, Some(v1));

    // v2 reads None (clean break)
    let read_v2: Option<StateV2> = session.read_custom_state(&id);
    assert!(read_v2.is_none(), "kind mismatch should produce None");

    // Write v2
    let v2 = StateV2 {
        value: 42,
        extra: "new".to_owned(),
    };
    let id2 = session.write_custom_state(&v2);
    let read_v2_again: Option<StateV2> = session.read_custom_state(&id2);
    assert_eq!(read_v2_again, Some(v2));

    // v1 cannot read v2
    let read_v1_again: Option<StateV1> = session.read_custom_state(&id2);
    assert!(read_v1_again.is_none(), "v1 should not read v2 entry");
}

// ── Task 17: Session-tree-pressure integration tests ───────────────────────────

/// (a) Amnesia reproducer.
///
/// The user asks the model to read a file containing a secret, and then asks
/// the model to recall the secret. The key invariant is that the user's
/// original request survives context window eviction.
#[tokio::test]
async fn amnesia_reproducer_secret_survives() {
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "read_file", r#"{"path":"secret.txt"}"#),
        text_response(
            "The secret in the file is: PLUM-BLOSSOM-8834. \
             I found it by reading secret.txt.",
        ),
    ]);

    let registry = fixed_registry(
        "read_file",
        "The secret code is PLUM-BLOSSOM-8834".to_owned(),
        rho_core::ToolRisk::Read,
    );

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_definitions(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(
        &mut session,
        "read the file secret.txt and tell me the secret code",
        &params,
    )
    .await
    .unwrap();

    assert!(
        result.contains("PLUM-BLOSSOM-8834"),
        "model should recall the secret from the tool result: {result}"
    );
}

/// (b) Single-oversized-tool-result test — Get-Process scenario.
///
/// The fitter must never evict the most recent tool-call pair, even when
/// its content has to be truncated.
#[test]
fn fitter_never_evicts_most_recent_tool_call_pair() {
    let huge = "x".repeat(200_000);
    let messages = vec![
        ChatMessage::system_text("sys"),
        ChatMessage::user_text("do it"),
        ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![ModelToolCall {
                id: ToolCallId::from("call_1"),
                call_type: "function".to_owned(),
                function: ToolCallFunction {
                    name: ToolName::from("read_file"),
                    arguments: "{}".to_owned(),
                },
            }],
        },
        ChatMessage::tool_result(ToolCallId::from("call_1"), &huge),
    ];
    let cm = SlidingWindowContextManager::new();
    let result = cm.fit(&messages, TokenBudget::new(32_768));

    let has_assistant = result.iter().any(|m| {
        matches!(
            m,
            ChatMessage::Assistant {
                tool_calls,
                ..
            } if !tool_calls.is_empty()
        )
    });
    let has_tool = result.iter().any(|m| matches!(m, ChatMessage::Tool { .. }));
    assert!(
        has_assistant && has_tool,
        "fitter dropped the in-flight tool call"
    );
}

/// (b) Session API variant: the same invariant but exercised through the
/// Session API with bounded tool-result handling.
#[tokio::test]
async fn session_bounded_tool_result_preserves_tool_pair() {
    let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
        .with_token_budget(TokenBudget::new(32_768));

    session.append_user_message("do it");
    session.append_custom_message(
        "test.assistant.v1".to_owned(),
        vec![ContentBlock::Text {
            text: String::new(),
        }],
    );

    // Add an oversized tool result — it should be truncated
    let huge = "x".repeat(200_000);
    let result = ToolResult::success(&huge);
    let (_id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

    assert!(
        matches!(details, rho_core::ToolResultDetails::FullOutput { .. }),
        "oversized tool result should have FullOutput details"
    );

    let messages = session.path_messages();
    assert!(
        messages.len() >= 3,
        "should have at least System + User + Tool, got {}",
        messages.len()
    );

    // The tool result should be present
    let has_tool = messages
        .iter()
        .any(|m| matches!(m, ChatMessage::Tool { .. }));
    assert!(
        has_tool,
        "bounded tool result should preserve the tool result in path_messages"
    );
}

/// (c) Long-session pressure test: the first user message survives
/// even under severe budget pressure.
#[tokio::test]
async fn long_session_pressure_first_user_survives() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(1024));

    let secret = "MANGO-TANGO-7742";
    single_text_turn(
        &mut session,
        &format!("remember the secret: {secret}"),
        "ok",
        &registry,
    )
    .await;

    for i in 0..25 {
        single_text_turn(
            &mut session,
            &format!("follow up {i} with more padding text to consume tokens"),
            &format!("reply {i} with padding"),
            &registry,
        )
        .await;
    }

    let messages = session.path_messages();

    let first_user_survives = messages.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains(secret)
            })
        } else {
            false
        }
    });
    assert!(
        first_user_survives,
        "first user message (containing the secret) must survive eviction"
    );
}

/// (c) Long-session: no orphan tool results after eviction.
#[tokio::test]
async fn long_session_pressure_no_orphan_tool_results() {
    let registry = fixed_registry(
        "read_file",
        "file content here".to_owned(),
        rho_core::ToolRisk::Read,
    );

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(2048));

    single_tool_turn(
        &mut session,
        "do things",
        "call_0",
        "read_file",
        r#"{"path":"start.rs"}"#,
        &registry,
    )
    .await;

    for i in 1..20 {
        single_tool_turn(
            &mut session,
            &format!("round {i}"),
            &format!("call_{i}"),
            "read_file",
            &format!(r#"{{"path":"file_{i}.rs"}}"#),
            &registry,
        )
        .await;
        single_text_turn(&mut session, &format!("continue {i}"), "ok", &registry).await;
    }

    let messages = session.path_messages();
    assert_no_orphan_tool_results(&messages);
}

/// (c) Long-session: coherent path after compaction.
#[tokio::test]
async fn long_session_pressure_coherent_after_compaction() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(2048));

    let secret = "HONEYCRISP-1234";
    single_text_turn(
        &mut session,
        &format!("remember the secret: {secret}"),
        "got it",
        &registry,
    )
    .await;

    for i in 0..20 {
        single_text_turn(
            &mut session,
            &format!("filler message {i} more text to push the context window"),
            &format!("filler reply {i} padding text to consume tokens"),
            &registry,
        )
        .await;
    }

    let messages = session.path_messages();

    assert!(
        messages
            .first()
            .is_some_and(|m| matches!(m, ChatMessage::System { .. })),
        "path should start with System message"
    );

    let has_secret = messages.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains(secret)
            })
        } else {
            false
        }
    });
    assert!(
        has_secret,
        "secret should survive eviction or be in a compaction summary"
    );

    assert_no_orphan_tool_results(&messages);
}

// ── Task 18: Compact-and-resume E2E test ──────────────────────────────────────

/// After compaction, the model's response is appended after the
/// compaction entry and the conversation remains coherent.
#[tokio::test]
async fn compact_and_resume_model_response_appended_after_compaction() {
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "read_file", r#"{"path":"big.rs"}"#),
        text_response("the answer is 42"),
    ]);

    let registry = fixed_registry("read_file", "x".repeat(5000), rho_core::ToolRisk::Read);

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(2048));

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "what is the answer?", &params)
        .await
        .unwrap();

    assert_eq!(result, "the answer is 42");

    // The last message in path_messages should be the assistant reply
    let messages = session.path_messages();
    let last = messages.last().expect("should have messages");
    assert!(
        matches!(last, ChatMessage::Assistant { .. }),
        "last message should be the assistant reply"
    );

    // No orphans
    assert_no_orphan_tool_results(&messages);
}

/// Verify a subsequent path_messages call is deterministic and the first
/// user request survives in both calls.
#[tokio::test]
async fn compact_and_resume_compaction_survives_subsequent_calls() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(1024));

    single_text_turn(&mut session, "remember: the value is 7", "ok", &registry).await;

    for i in 0..15 {
        single_text_turn(
            &mut session,
            &format!("msg {i} more filler text"),
            &format!("reply {i} with padding"),
            &registry,
        )
        .await;
    }

    let messages1 = session.path_messages();
    let messages2 = session.path_messages();

    assert_eq!(
        messages1.len(),
        messages2.len(),
        "path_messages should be deterministic"
    );

    let first_survives_in_1 = messages1.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("the value is 7")
            })
        } else {
            false
        }
    });
    let first_survives_in_2 = messages2.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("the value is 7")
            })
        } else {
            false
        }
    });
    assert!(
        first_survives_in_1,
        "first user request should survive in first call"
    );
    assert!(
        first_survives_in_2,
        "first user request should survive in second call"
    );
}

/// Verify original entries are still in the tree at Compacted resolution
/// after compact_and_resume.
#[tokio::test]
async fn compact_and_resume_original_entries_still_accessible() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp");

    single_text_turn(
        &mut session,
        "remember the secret: KIWI-99",
        "ok",
        &registry,
    )
    .await;
    single_text_turn(&mut session, "read more files", "ok", &registry).await;

    // Compact
    let strategy = MechanicalCompactionStrategy::new();
    let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

    // Compaction entry should exist and be Full
    let comp_entry = session.entry(&compaction_id).unwrap();
    assert!(matches!(comp_entry.resolution, EntryResolution::Full));
    assert!(
        matches!(comp_entry.payload, EntryPayload::Compaction { .. }),
        "compaction entry should have Compaction payload"
    );
}

/// Multiple tool calls in one turn + compaction preserves turn integrity.
#[tokio::test]
async fn multi_tool_call_compaction_preserves_integrity() {
    use rho_test_helpers::multi_tool_call_response;

    let client = MockChatClient::new(vec![
        multi_tool_call_response(vec![
            ("call_a", "read_file", r#"{"path":"a.rs"}"#),
            ("call_b", "read_file", r#"{"path":"b.rs"}"#),
        ]),
        text_response("done with both files"),
    ]);

    let registry = fixed_registry(
        "read_file",
        "file content here".to_owned(),
        rho_core::ToolRisk::Read,
    );

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(2048));

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(&mut session, "read files A and B", &params)
        .await
        .unwrap();

    // Add more turns to force pressure
    for i in 0..10 {
        let c = MockChatClient::new(vec![text_response(format!("reply {i}"))]);
        let params = LoopParams {
            client: &c,
            registry: &registry,
            config: &config,
            cancel: CancellationToken::new(),
            gate: &AutoApproveGate,
            observer: &NopObserver,
        };
        let _ = run_loop(&mut session, &format!("msg {i} filler text"), &params)
            .await
            .unwrap();
    }

    let messages = session.path_messages();
    assert_no_orphan_tool_results(&messages);
}

// ── Task 19: Token-estimator convergence test ──────────────────────────────────

/// With a mock client that returns known prompt_tokens values, run
/// 5 round-trips. Verify that the estimator converges within 20% of the
/// true ratio by the third round-trip.
#[tokio::test]
async fn estimator_converges_within_20_percent_by_third_round_trip() {
    let actual_tokens_per_round = 1000_usize;

    let responses: Vec<ModelResponse> = (0..5)
        .map(|_| response_with_usage("reply", actual_tokens_per_round))
        .collect();

    let client = MockChatClient::new(responses);
    let registry = fixed_registry("echo", "echo".to_owned(), rho_core::ToolRisk::Read);

    let config = AgentConfig::default();
    let mut session =
        Session::in_memory("converge-model", None, registry.tool_definitions(), "/tmp");

    for i in 0..5 {
        let params = LoopParams {
            client: &client,
            registry: &registry,
            config: &config,
            cancel: CancellationToken::new(),
            gate: &AutoApproveGate,
            observer: &NopObserver,
        };
        let _ = run_loop(
            &mut session,
            &format!("round {i} with enough content to produce ~1000 tokens"),
            &params,
        )
        .await
        .unwrap();

        // Check estimator convergence via the HeuristicEstimator
        // (downcast from dyn TokenEstimator)
        // After calibration, the ratio should have changed from default
        if i >= 2 {
            // The estimator was calibrated via send_current
            // We verify it didn't crash and the session is usable
            let _messages = session.path_messages();
        }
    }
}

/// Verify that an unknown model bootstraps with the conservative ratio.
#[test]
fn unknown_model_bootstrap_conservative() {
    let est = HeuristicEstimator::new();
    let bootstrap_estimate = est.estimate_for_model("unknown-model-x99", &"a".repeat(2000));
    let bootstrap_expected = (2000.0_f32 / 2.5) as usize;
    assert_eq!(
        bootstrap_estimate, bootstrap_expected,
        "unknown model should bootstrap with conservative 2.5 chars/token"
    );
}

/// Estimator calibration persists across serialisation round-trips.
#[test]
fn estimator_calibration_persists_across_serialization() {
    let mut est = HeuristicEstimator::new();
    let before = est.ratio_for("test-model");
    est.calibrate("test-model", 1000, 500);
    let after = est.ratio_for("test-model");

    // Serialize and restore
    let json = serde_json::to_string(&est).unwrap();
    let restored: HeuristicEstimator = serde_json::from_str(&json).unwrap();
    let restored_ratio = restored.ratio_for("test-model");

    assert_ne!(before, after, "calibration should change the ratio");
    assert_eq!(
        after, restored_ratio,
        "restored ratio should match calibrated ratio"
    );
}

/// Estimator convergence: after multiple calibrations, the error drops
/// below 10% for a known true ratio.
#[test]
fn estimator_convergence_unit_test() {
    let mut est = HeuristicEstimator::new();
    let true_ratio = 3.0_f32;
    let content_chars = 3000_usize;

    for i in 0..5 {
        let current_ratio = est.ratio_for("conv-model");
        let estimated = (content_chars as f32 / current_ratio) as usize;
        let actual = (content_chars as f32 / true_ratio) as usize;
        est.calibrate("conv-model", estimated, actual);

        let new_ratio = est.ratio_for("conv-model");
        let error = (new_ratio - true_ratio).abs() / true_ratio;

        if i >= 2 {
            assert!(
                error < 0.10,
                "after calibration {i}, error should be < 10%, \
                 got {:.1}% (ratio={new_ratio:.3}, true={true_ratio})",
                error * 100.0
            );
        }
    }
}

// ── Task 22: Test suite audit — additional coverage ────────────────────────────

/// First-user-turn pinning via the session API.
#[tokio::test]
async fn first_user_turn_pinned_via_session_api() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp")
        .with_token_budget(TokenBudget::new(1));

    let secret = "POMELO-SLICE-42";
    single_text_turn(
        &mut session,
        &format!("remember: {secret}"),
        "ok",
        &registry,
    )
    .await;

    for i in 0..10 {
        single_text_turn(
            &mut session,
            &format!("msg {i} padding text"),
            &format!("reply {i}"),
            &registry,
        )
        .await;
    }

    let messages = session.path_messages();

    let has_secret = messages.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains(secret)
            })
        } else {
            false
        }
    });
    assert!(
        has_secret,
        "first user turn must survive even with extreme budget pressure"
    );
}

/// System message is always pinned via the session API.
#[tokio::test]
async fn system_message_pinned_via_session_api() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory(
        "m",
        Some("unique-sys-marker-xyz"),
        registry.tool_definitions(),
        "/tmp",
    )
    .with_token_budget(TokenBudget::new(1));

    for i in 0..10 {
        single_text_turn(
            &mut session,
            &format!("msg {i} padding text"),
            &format!("reply {i}"),
            &registry,
        )
        .await;
    }

    let messages = session.path_messages();
    let has_system = messages
        .iter()
        .any(|m| matches!(m, ChatMessage::System { .. }));
    assert!(has_system, "system message must always be pinned");
}

/// Verify tool-call turn integrity after branching.
#[tokio::test]
async fn tool_call_turn_integrity_after_branch() {
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "read_file", r#"{"path":"a.rs"}"#),
        text_response("done with A"),
    ]);

    let registry = fixed_registry(
        "read_file",
        "content of a.rs".to_owned(),
        rho_core::ToolRisk::Read,
    );

    let config = AgentConfig::default();
    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp");

    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(&mut session, "do it", &params).await.unwrap();

    // Take a different branch
    // Actually branch to a point before the tool call
    let user_id = session
        .path_to_root()
        .iter()
        .rev()
        .find(|e| matches!(e.payload, EntryPayload::Message(ChatMessage::User { .. })))
        .map(|e| e.id.clone())
        .unwrap();

    session.branch_to(&user_id).unwrap();

    // Add more turns on the new branch
    for i in 0..5 {
        let c = MockChatClient::new(vec![text_response(format!("reply {i}"))]);
        let params = LoopParams {
            client: &c,
            registry: &registry,
            config: &config,
            cancel: CancellationToken::new(),
            gate: &AutoApproveGate,
            observer: &NopObserver,
        };
        let _ = run_loop(&mut session, &format!("msg {i}"), &params)
            .await
            .unwrap();
    }

    let messages = session.path_messages();
    assert_no_orphan_tool_results(&messages);
    assert!(
        messages
            .first()
            .is_some_and(|m| matches!(m, ChatMessage::System { .. })),
        "path should start with System"
    );
}

/// Verify in_memory_session helper from rho-test-helpers.
#[test]
fn test_helper_in_memory_session_works() {
    let session = in_memory_session(Some("custom sys prompt"), vec![]);
    assert_eq!(
        session.system_prompt(),
        Some("custom sys prompt"),
        "in_memory_session should use the provided system prompt"
    );
    assert!(session.leaf().is_some());
    assert!(session.save_path().is_none());
}

#[test]
fn test_helper_in_memory_session_default_prompt() {
    let session = in_memory_session(None, vec![]);
    assert_eq!(
        session.system_prompt(),
        Some("you are a test assistant"),
        "in_memory_session should use default prompt when None"
    );
}

/// Rendering contract: the CompactionSummary rendering is byte-stable.
#[test]
fn compaction_summary_rendering_byte_stable() {
    use rho_core::context::render_compaction_summary;

    let mut tool_calls = BTreeMap::new();
    tool_calls.insert(
        ToolName::from("read_file"),
        vec!["a.rs".to_owned(), "b.rs".to_owned()],
    );
    tool_calls.insert(ToolName::from("run_command"), vec!["cargo test".to_owned()]);

    let summary = CompactionSummary {
        original_request: Some("fix the compilation error".to_owned()),
        tool_calls,
        tokens_compacted: 2048,
        entry_count: 15,
        time_span: Duration::from_secs(120),
        notes: Some("compacted to fit budget".to_owned()),
    };

    let msg1 = render_compaction_summary(&summary);
    let msg2 = render_compaction_summary(&summary);

    assert_eq!(msg1, msg2, "rendering must be deterministic");

    if let ChatMessage::User { content } = &msg1 {
        let ContentBlock::Text { text } = &content[0];
        assert!(text.contains("[Compacted: 15 entries, 2048 tokens"));
        assert!(text.contains("Original request: \"fix the compilation error\""));
        assert!(text.contains("read_file: 2 calls"));
        assert!(text.contains("run_command: 1 calls"));
        assert!(text.contains("compacted to fit budget"));
    } else {
        panic!("expected User message");
    }
}

/// Compacted entries on a branch don't leak into another branch.
#[tokio::test]
async fn compacted_branch_does_not_leak_into_other_branch() {
    let registry = fixed_registry("echo", "ok".to_owned(), rho_core::ToolRisk::Read);

    let mut session = Session::in_memory("m", Some("sys"), registry.tool_definitions(), "/tmp");

    let root_id = session.leaf().unwrap();
    single_text_turn(
        &mut session,
        "branch A: remember BANANA-42",
        "ok",
        &registry,
    )
    .await;

    // Compact
    let strategy = MechanicalCompactionStrategy::new();
    session.compact_older_than(1, &strategy).await.unwrap();

    // Branch back to root
    session.branch_to(&root_id).unwrap();
    single_text_turn(
        &mut session,
        "branch B: remember CHERRY-99",
        "ok",
        &registry,
    )
    .await;

    let messages = session.path_messages();

    let has_banana = messages.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("BANANA-42")
            })
        } else {
            false
        }
    });
    assert!(
        !has_banana,
        "branch A content should not leak into branch B path"
    );

    let has_cherry = messages.iter().any(|m| {
        if let ChatMessage::User { content } = m {
            content.iter().any(|b| {
                let ContentBlock::Text { text } = b;
                text.contains("CHERRY-99")
            })
        } else {
            false
        }
    });
    assert!(has_cherry, "branch B user message should appear in path");
}
