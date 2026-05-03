//! Session tree entry types.
//!
//! The session tree is the core data structure of Phase 2.5. Entries form a
//! parent-linked tree with a movable leaf pointer. Each entry carries an
//! explicit [`EntryResolution`] that determines whether it participates in the
//! model's context — this is the *adaptive resolution* framing: preserve fine
//! detail where it matters, coarsen where it doesn't.
//!
//! # Resolution and LLM context
//!
//! | Payload variant | Default resolution | In LLM context? |
//! |---|---|---|
//! | [`Message`](EntryPayload::Message) | Full | Yes (unless Compacted/Attached) |
//! | [`Compaction`](EntryPayload::Compaction) | Full | Yes — rendered as synthetic user message |
//! | [`BranchSummary`](EntryPayload::BranchSummary) | Full | Yes — rendered as synthetic user message |
//! | [`ModelChange`](EntryPayload::ModelChange) | Attached | No (metadata only) |
//! | [`Label`](EntryPayload::Label) | Attached | No (metadata only) |
//! | [`SessionInfo`](EntryPayload::SessionInfo) | Attached | No (metadata only) |
//! | [`LeafMoved`](EntryPayload::LeafMoved) | Attached | No (audit trail only) |
//! | [`Custom`](EntryPayload::Custom) | Attached | No (extension state) |
//! | [`CustomMessage`](EntryPayload::CustomMessage) | Full | Yes (extension content) |

use crate::message::{ChatMessage, ContentBlock};
use crate::newtypes::{EntryId, ToolName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

// ── Entry ─────────────────────────────────────────────────────────────────────

/// A single node in the session tree.
///
/// Every entry has an explicit [`EntryResolution`] that determines whether it
/// participates in the model's context. Entries are linked by `parent_id` to
/// form a tree; the session's `leaf` pointer identifies the current position.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Entry {
    /// Unique identifier for this entry.
    pub id: EntryId,
    /// Parent entry in the tree. `None` for the root (system message).
    pub parent_id: Option<EntryId>,
    /// When this entry was created.
    pub timestamp: SystemTime,
    /// Current resolution level — controls visibility to the model.
    pub resolution: EntryResolution,
    /// The typed payload — what this entry actually contains.
    pub payload: EntryPayload,
}

// ── EntryResolution ───────────────────────────────────────────────────────────

/// The resolution level of an entry.
///
/// This is the core of the *adaptive resolution* framing. An entry's resolution
/// determines whether it participates in the model's context:
///
/// - **Full** — the entry's content is sent to the model as-is.
/// - **Compacted** — the entry has been summarised into another entry
///   (`into`); the original is preserved in the tree but bypassed by
///   `fit_path`.
/// - **Attached** — the entry is preserved for tools and extensions to read,
///   but does not participate in the model's context.
///
/// Resolution transitions are one-way in Phase 2.5: `Full → Compacted` or
/// `Full → Attached`. Reverse transitions may be supported in a future phase
/// for branching scenarios.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum EntryResolution {
    /// Full content participates in the model's context.
    Full,
    /// Content has been summarised into another entry; the original is
    /// bypassed by `fit_path` but still accessible via `entry(id)`.
    Compacted {
        /// The ID of the `Compaction` or `BranchSummary` entry that replaces
        /// this one in the model's context.
        into: EntryId,
    },
    /// Content is preserved as structured detail but doesn't participate in
    /// the model's context. Tools can read it; the LLM doesn't see it.
    Attached,
}

// ── EntryPayload ──────────────────────────────────────────────────────────────

/// The typed content of a session entry.
///
/// Each variant documents its default [`EntryResolution`] and whether it
/// participates in the LLM context at that default resolution.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum EntryPayload {
    /// A conversation message (system, user, assistant, or tool result).
    ///
    /// Default resolution: **Full**. Participates in LLM context.
    Message(ChatMessage),

    /// A compaction entry summarising a range of older entries.
    ///
    /// Default resolution: **Full**. Rendered as a synthetic `User` message by
    /// `fit_path`. The `first_kept` field identifies the first entry *after*
    /// the compacted range — entries between the compaction's parent and
    /// `first_kept` have their resolution transitioned to `Compacted { into:
    /// self.id }`.
    Compaction {
        summary: CompactionSummary,
        first_kept: EntryId,
        tokens_before: usize,
    },

    /// A summary of an abandoned branch, inserted when the leaf moves to a
    /// different position in the tree.
    ///
    /// Default resolution: **Full**. Rendered as a synthetic `User` message by
    /// `fit_path`.
    BranchSummary {
        summary: CompactionSummary,
        from_id: EntryId,
    },

    /// Records a model change mid-session.
    ///
    /// Default resolution: **Attached**. Does NOT participate in LLM context.
    /// The calibrator uses this to track which model was active at each point.
    ModelChange { model: String },

    /// A label attached to another entry (for bookmarking / naming).
    ///
    /// Default resolution: **Attached**. Does NOT participate in LLM context.
    Label {
        target_id: EntryId,
        label: Option<String>,
    },

    /// Session-level metadata (e.g. session name).
    ///
    /// Default resolution: **Attached**. Does NOT participate in LLM context.
    SessionInfo { name: String },

    /// Records a leaf-pointer move (audit trail for branch operations).
    ///
    /// Default resolution: **Attached**. Does NOT participate in LLM context.
    /// Written only when the leaf moves to a non-adjacent position (i.e.,
    /// during branch operations, not during normal append).
    LeafMoved { from: Option<EntryId>, to: EntryId },

    /// Extension state that does NOT become part of the LLM context.
    ///
    /// Default resolution: **Attached**. `kind` is namespaced following the
    /// `ExtensionEntry::KIND` convention (e.g., `"rho.diagnostics.v1"`).
    Custom {
        kind: String,
        data: serde_json::Value,
    },

    /// Extension content that DOES become part of the LLM context.
    ///
    /// Default resolution: **Full**. `kind` is namespaced following the
    /// `ExtensionEntry::KIND` convention. The `content` is rendered as part
    /// of the message path sent to the model.
    CustomMessage {
        kind: String,
        content: Vec<ContentBlock>,
    },
}

// ── CompactionSummary ─────────────────────────────────────────────────────────

/// A structured summary produced by a [`CompactionStrategy`](crate::session::CompactionStrategy).
///
/// This is the data that `fit_path` renders into a synthetic `User` message
/// when a `Compaction` or `BranchSummary` entry appears on the leaf path.
///
/// The structured form (as opposed to prose) ensures:
/// - P2.5-6: the original user request is preserved by type, not convention.
/// - P2.5-8: what was evicted is self-describing.
/// - An `LlmCompactionStrategy` (Phase 4+) can populate `notes` without
///   changing the consumers.
///
/// The rendering contract is:
/// ```text
/// [Compacted: {entry_count} entries, {tokens_compacted} tokens, span {duration}]
/// Original request: "{original_request, if present}"
/// Tool activity:
///   - {tool_name}: {N} calls — {args_summary_1}, {args_summary_2}, ...
/// {notes, if present}
/// ```
///
/// **Note:** The full `CompactionStrategy` trait and `MechanicalCompactionStrategy`
/// implementation are Task 9. This struct definition is provided here so that
/// `EntryPayload::Compaction` and `EntryPayload::BranchSummary` are constructable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionSummary {
    /// Verbatim original user request, if present in the compacted range.
    pub original_request: Option<String>,
    /// Tool calls grouped by tool name, with one-line argument summaries.
    pub tool_calls: BTreeMap<ToolName, Vec<String>>,
    /// Total estimated tokens across all compacted entries.
    pub tokens_compacted: usize,
    /// Number of entries that were compacted.
    pub entry_count: usize,
    /// Wall-clock span from the first to the last compacted entry.
    pub time_span: Duration,
    /// Optional free-form notes (used by LLM strategies; empty for mechanical).
    pub notes: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ModelToolCall, ToolCallFunction};
    use crate::newtypes::ToolCallId;
    use std::collections::BTreeMap;
    use std::time::Duration;

    /// Helper: create a minimal `Entry` for testing.
    fn test_entry(payload: EntryPayload, resolution: EntryResolution) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution,
            payload,
        }
    }

    // ── EntryResolution round-trips ────────────────────────────────────────

    #[test]
    fn resolution_full_round_trips() {
        let res = EntryResolution::Full;
        let json = serde_json::to_string(&res).unwrap();
        let back: EntryResolution = serde_json::from_str(&json).unwrap();
        assert_eq!(res, back);
    }

    #[test]
    fn resolution_compacted_round_trips() {
        let res = EntryResolution::Compacted {
            into: EntryId::new(),
        };
        let json = serde_json::to_string(&res).unwrap();
        let back: EntryResolution = serde_json::from_str(&json).unwrap();
        assert_eq!(res, back);
    }

    #[test]
    fn resolution_attached_round_trips() {
        let res = EntryResolution::Attached;
        let json = serde_json::to_string(&res).unwrap();
        let back: EntryResolution = serde_json::from_str(&json).unwrap();
        assert_eq!(res, back);
    }

    // ── EntryPayload round-trips ───────────────────────────────────────────

    #[test]
    fn payload_message_round_trips() {
        let payload = EntryPayload::Message(ChatMessage::user_text("hello"));
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_compaction_round_trips() {
        let mut tool_calls = BTreeMap::new();
        tool_calls.insert(ToolName::from("read_file"), vec!["src/main.rs".to_owned()]);
        let summary = CompactionSummary {
            original_request: Some("list files".to_owned()),
            tool_calls,
            tokens_compacted: 1024,
            entry_count: 5,
            time_span: Duration::from_secs(30),
            notes: None,
        };
        let payload = EntryPayload::Compaction {
            summary,
            first_kept: EntryId::new(),
            tokens_before: 2048,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_branch_summary_round_trips() {
        let summary = CompactionSummary {
            original_request: None,
            tool_calls: BTreeMap::new(),
            tokens_compacted: 512,
            entry_count: 3,
            time_span: Duration::from_secs(10),
            notes: None,
        };
        let payload = EntryPayload::BranchSummary {
            summary,
            from_id: EntryId::new(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_model_change_round_trips() {
        let payload = EntryPayload::ModelChange {
            model: "gpt-4".to_owned(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_label_round_trips() {
        let payload = EntryPayload::Label {
            target_id: EntryId::new(),
            label: Some("checkpoint".to_owned()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_label_none_round_trips() {
        let payload = EntryPayload::Label {
            target_id: EntryId::new(),
            label: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_session_info_round_trips() {
        let payload = EntryPayload::SessionInfo {
            name: "my session".to_owned(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_leaf_moved_round_trips() {
        let payload = EntryPayload::LeafMoved {
            from: Some(EntryId::new()),
            to: EntryId::new(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_leaf_moved_from_none_round_trips() {
        let payload = EntryPayload::LeafMoved {
            from: None,
            to: EntryId::new(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_custom_round_trips() {
        let payload = EntryPayload::Custom {
            kind: "rho.diagnostics.v1".to_owned(),
            data: serde_json::json!({"errors": 3}),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    #[test]
    fn payload_custom_message_round_trips() {
        let payload = EntryPayload::CustomMessage {
            kind: "rho.diagnostics.v1".to_owned(),
            content: vec![ContentBlock::Text {
                text: "3 errors found".to_owned(),
            }],
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: EntryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, back);
    }

    // ── Full Entry round-trips ─────────────────────────────────────────────

    #[test]
    fn entry_with_message_round_trips() {
        let entry = test_entry(
            EntryPayload::Message(ChatMessage::assistant_text("done")),
            EntryResolution::Full,
        );
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn entry_with_compacted_resolution_round_trips() {
        let compacted_into = EntryId::new();
        let entry = test_entry(
            EntryPayload::Message(ChatMessage::user_text("old")),
            EntryResolution::Compacted {
                into: compacted_into.clone(),
            },
        );
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn entry_with_attached_resolution_round_trips() {
        let entry = test_entry(
            EntryPayload::ModelChange {
                model: "qwen".to_owned(),
            },
            EntryResolution::Attached,
        );
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    #[test]
    fn entry_with_parent_round_trips() {
        let parent = EntryId::new();
        let entry = Entry {
            id: EntryId::new(),
            parent_id: Some(parent),
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::user_text("follow-up")),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    // ── CompactionSummary round-trips ──────────────────────────────────────

    #[test]
    fn compaction_summary_round_trips() {
        let mut tool_calls = BTreeMap::new();
        tool_calls.insert(
            ToolName::from("read_file"),
            vec!["a.rs".to_owned(), "b.rs".to_owned()],
        );
        tool_calls.insert(ToolName::from("run_command"), vec!["cargo test".to_owned()]);
        let summary = CompactionSummary {
            original_request: Some("fix the bug".to_owned()),
            tool_calls,
            tokens_compacted: 4096,
            entry_count: 12,
            time_span: Duration::from_mins(2),
            notes: Some("LLM notes here".to_owned()),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: CompactionSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, back);
    }

    #[test]
    fn compaction_summary_minimal_round_trips() {
        let summary = CompactionSummary {
            original_request: None,
            tool_calls: BTreeMap::new(),
            tokens_compacted: 0,
            entry_count: 0,
            time_span: Duration::ZERO,
            notes: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: CompactionSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, back);
    }

    // ── Cross-variant entry with assistant tool calls ──────────────────────

    #[test]
    fn entry_with_assistant_tool_calls_round_trips() {
        let entry = test_entry(
            EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
                    },
                }],
            }),
            EntryResolution::Full,
        );
        let json = serde_json::to_string(&entry).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }
}
