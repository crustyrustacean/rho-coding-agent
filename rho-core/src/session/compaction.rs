//! Compaction strategies for adaptive-resolution context management.
//!
//! When the session tree grows beyond the token budget, older entries can be
//! *compacted* — summarised into a [`CompactionSummary`] and transitioned from
//! [`Full`](EntryResolution::Full) to
//! [`Compacted`](EntryResolution::Compacted) resolution. The original entries
//! remain in the tree (accessible via [`Session::entry`]), but are bypassed by
//! [`fit_path`](ContextManager::fit_path), which instead renders the summary as
//! a synthetic `User` message.
//!
//! # Strategy trait
//!
//! [`CompactionStrategy`] is the extension point. Phase 2.5 ships
//! [`MechanicalCompactionStrategy`], which produces deterministic, structured
//! summaries without any LLM calls. Phase 4+ may introduce
//! `LlmCompactionStrategy` that populates the `notes` field with model-generated
//! prose.
//!
//! # Compaction vs. deletion
//!
//! Compaction is a *refinement* operation, not a deletion. The compacted entries
//! stay in the tree at lower resolution. This is the core of the adaptive-
//! resolution framing: keep fine detail where it matters, coarsen where it
//! doesn't.
//!
//! [`Session::entry`]: crate::session::Session::entry

use crate::error::Result;
use crate::message::ChatMessage;
use crate::newtypes::ToolName;
use crate::session::entry::{CompactionSummary, Entry, EntryPayload};
use std::collections::BTreeMap;
use std::time::Duration;

// ── CompactionStrategy trait ──────────────────────────────────────────────────

/// Strategy for producing a structured summary of a range of session entries.
///
/// Implementations range from deterministic mechanical summarisation (no LLM
/// calls) to model-driven summarisation. The trait is `Send + Sync` so it can
/// be shared across async tasks.
///
/// # Contract
///
/// - The input entries are in **chronological order** (oldest first).
/// - The returned [`CompactionSummary`] must accurately reflect the input range.
/// - [`MechanicalCompactionStrategy`] is the default and produces deterministic
///   output for a fixed input — no randomness, no LLM calls.
#[async_trait::async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Produce a structured summary of the given entries.
    ///
    /// The entries are guaranteed to be in chronological order and all have
    /// resolution [`Full`](crate::session::EntryResolution::Full) — the caller
    /// filters before passing them.
    async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary>;
}

// ── MechanicalCompactionStrategy ──────────────────────────────────────────────

/// A deterministic compaction strategy that produces structured summaries
/// without any LLM calls.
///
/// Walks the entries and extracts:
/// - `original_request`: the first `ChatMessage::User` text encountered.
/// - `tool_calls`: groups `ChatMessage::Assistant { tool_calls }` entries by
///   tool name; for each call, formats a one-line argument summary.
/// - `tokens_compacted`: sum of estimated tokens across all compacted entries.
/// - `entry_count`, `time_span`: trivial walks.
/// - `notes`: always `None` (LLM strategies populate this).
///
/// The output is deterministic for a fixed input — no randomness, no external
/// calls. This makes it suitable for testing and for cases where model
/// summarisation is unavailable or too expensive.
#[derive(Default)]
pub struct MechanicalCompactionStrategy {
    /// Token estimator for computing `tokens_compacted`.
    /// Uses a simple chars/4 heuristic; the session's calibrated estimator
    /// is not needed here because compaction is an approximate operation.
    _phantom: (),
}

impl MechanicalCompactionStrategy {
    /// Create a new mechanical compaction strategy.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl CompactionStrategy for MechanicalCompactionStrategy {
    async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary> {
        let mut original_request: Option<String> = None;
        let mut tool_calls: BTreeMap<ToolName, Vec<String>> = BTreeMap::new();
        let mut tokens_compacted: usize = 0;
        let mut first_timestamp: Option<std::time::SystemTime> = None;
        let mut last_timestamp: Option<std::time::SystemTime> = None;

        for entry in entries {
            // Track time span
            if first_timestamp.is_none() || entry.timestamp < first_timestamp.unwrap() {
                first_timestamp = Some(entry.timestamp);
            }
            if last_timestamp.is_none() || entry.timestamp > last_timestamp.unwrap() {
                last_timestamp = Some(entry.timestamp);
            }

            // Estimate tokens for this entry
            tokens_compacted += estimate_entry_tokens(entry);

            if let EntryPayload::Message(msg) = &entry.payload {
                match msg {
                    ChatMessage::User { content } => {
                        // Capture the first user message as the original request
                        if original_request.is_none() {
                            let text = extract_text(content);
                            if !text.is_empty() {
                                original_request = Some(text);
                            }
                        }
                    }
                    ChatMessage::Assistant {
                        tool_calls: calls, ..
                    } => {
                        for call in calls {
                            tool_calls
                                .entry(call.function.name.clone())
                                .or_default()
                                .push(summarise_arguments(&call.function.arguments));
                        }
                    }
                    ChatMessage::System { .. } | ChatMessage::Tool { .. } => {}
                }
            }
        }

        let time_span = match (first_timestamp, last_timestamp) {
            (Some(first), Some(last)) => last.duration_since(first).unwrap_or(Duration::ZERO),
            _ => Duration::ZERO,
        };

        Ok(CompactionSummary {
            original_request,
            tool_calls,
            tokens_compacted,
            entry_count: entries.len(),
            time_span,
            notes: None,
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Estimate the token count for an entry using the chars/4 heuristic.
///
/// This is deliberately simple — compaction doesn't need calibrated estimates.
/// The session's calibrated estimator is used for budget decisions (`fit_path`),
/// not for compaction summaries.
fn estimate_entry_tokens(entry: &Entry) -> usize {
    let chars = match &entry.payload {
        EntryPayload::Message(msg) => message_chars(msg),
        EntryPayload::Compaction {
            summary,
            tokens_before,
            ..
        } => {
            // For compaction entries, use tokens_before as the basis
            // plus the summary's own text content
            let summary_chars = summary_text_chars(summary);
            (*tokens_before / 4).max(summary_chars)
        }
        EntryPayload::BranchSummary { summary, .. } => summary_text_chars(summary),
        EntryPayload::Custom { data, .. } => data.to_string().len(),
        EntryPayload::CustomMessage { content, .. } => {
            content.iter().map(block_chars).sum::<usize>() + 20
        }
        EntryPayload::ModelChange { model } => model.len() + 20,
        EntryPayload::Label { label, .. } => label.as_ref().map_or(20, |l| l.len() + 20),
        EntryPayload::SessionInfo { name } => name.len() + 20,
        EntryPayload::LeafMoved { .. } => 40,
    };

    chars.div_ceil(4).max(1)
}

/// Count the approximate character content of a `ChatMessage`.
fn message_chars(msg: &ChatMessage) -> usize {
    use crate::message::ContentBlock;

    let mut chars = 20; // structural overhead
    match msg {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. } => {
            for block in content {
                chars += match block {
                    ContentBlock::Text { text } => text.len(),
                };
            }
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            chars += tool_call_id.len() + 10;
            for block in content {
                chars += match block {
                    ContentBlock::Text { text } => text.len(),
                };
            }
        }
    }
    chars
}

/// Count the character content of a `ContentBlock`.
fn block_chars(block: &crate::message::ContentBlock) -> usize {
    match block {
        crate::message::ContentBlock::Text { text } => text.len(),
    }
}

/// Count the approximate character content of a `CompactionSummary`.
fn summary_text_chars(summary: &CompactionSummary) -> usize {
    let mut chars = 100; // base overhead for header, etc.
    if let Some(ref req) = summary.original_request {
        chars += req.len();
    }
    for (name, calls) in &summary.tool_calls {
        chars += name.len() + calls.iter().map(|c| c.len() + 2).sum::<usize>();
    }
    if let Some(ref notes) = summary.notes {
        chars += notes.len();
    }
    chars
}

/// Extract the plain text from a slice of `ContentBlock`s.
fn extract_text(content: &[crate::message::ContentBlock]) -> String {
    content
        .iter()
        .map(|b| match b {
            crate::message::ContentBlock::Text { text } => text.as_str(),
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Produce a one-line argument summary from a JSON arguments string.
///
/// Tries to extract short, human-readable values from the JSON. For simple
/// string arguments, shows the value. For complex objects, shows a truncated
/// representation.
fn summarise_arguments(arguments: &str) -> String {
    // Try to parse as JSON and extract a short summary
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(arguments) {
        match &val {
            serde_json::Value::Object(map) => {
                // Show key=value pairs, truncated
                let pairs: Vec<String> = map
                    .iter()
                    .take(3) // at most 3 key-value pairs
                    .map(|(k, v)| {
                        let v_str = match v {
                            serde_json::Value::String(s) => {
                                if s.len() > 40 {
                                    format!("{}…", &s[..38])
                                } else {
                                    s.clone()
                                }
                            }
                            other => {
                                let s = other.to_string();
                                if s.len() > 40 {
                                    format!("{}…", &s[..38])
                                } else {
                                    s
                                }
                            }
                        };
                        format!("{k}={v_str}")
                    })
                    .collect();
                let summary = pairs.join(", ");
                if map.len() > 3 {
                    format!("{summary}, …")
                } else {
                    summary
                }
            }
            other => {
                let s = other.to_string();
                if s.len() > 60 {
                    format!("{}…", &s[..58])
                } else {
                    s
                }
            }
        }
    } else {
        // Not valid JSON — truncate the raw string
        if arguments.len() > 60 {
            format!("{}…", &arguments[..58])
        } else {
            arguments.to_owned()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    use super::*;
    use crate::message::{ChatMessage, ModelToolCall, ToolCallFunction};
    use crate::newtypes::{EntryId, ToolCallId, ToolName};
    use crate::session::entry::{Entry, EntryPayload, EntryResolution};
    use std::time::{Duration, SystemTime};

    /// Helper: create a test entry with the given payload and Full resolution.
    fn full_entry(payload: EntryPayload) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    /// Helper: create a test entry with a specific timestamp.
    fn timed_entry(payload: EntryPayload, ts: SystemTime) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: ts,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    // ── MechanicalCompactionStrategy tests ─────────────────────────────────

    #[tokio::test]
    async fn mechanical_extracts_original_request() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "fix the bug in main.rs",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text(
                "looking at it",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(
            summary.original_request,
            Some("fix the bug in main.rs".to_owned())
        );
    }

    #[tokio::test]
    async fn mechanical_original_request_is_first_user_message() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("hi"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("first"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("second"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        // Only the FIRST user message should be captured
        assert_eq!(summary.original_request, Some("first".to_owned()));
    }

    #[tokio::test]
    async fn mechanical_no_user_message_gives_none_original_request() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("hi"))),
            full_entry(EntryPayload::Message(ChatMessage::system_text("sys"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.original_request, None);
    }

    #[tokio::test]
    async fn mechanical_extracts_tool_calls() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "file contents",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/lib.rs"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        // Should have one tool (read_file) with 2 calls
        assert_eq!(summary.tool_calls.len(), 1);
        let calls = &summary.tool_calls[&ToolName::from("read_file")];
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("path=src/main.rs"));
        assert!(calls[1].contains("path=src/lib.rs"));
    }

    #[tokio::test]
    async fn mechanical_groups_multiple_tools() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"a.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "a",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("run_command"),
                        arguments: r#"{"command":"cargo test"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.tool_calls.len(), 2);
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("read_file"))
        );
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("run_command"))
        );
    }

    #[tokio::test]
    async fn mechanical_counts_entries() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("a"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("b"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("c"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.entry_count, 3);
    }

    #[tokio::test]
    async fn mechanical_computes_time_span() {
        let now = SystemTime::now();
        let entries = [
            timed_entry(EntryPayload::Message(ChatMessage::user_text("a")), now),
            timed_entry(
                EntryPayload::Message(ChatMessage::assistant_text("b")),
                now + Duration::from_secs(45),
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.time_span, Duration::from_secs(45));
    }

    #[tokio::test]
    async fn mechanical_empty_entries_gives_zero_summary() {
        let entries: Vec<&Entry> = vec![];

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&entries).await.unwrap();

        assert_eq!(summary.entry_count, 0);
        assert_eq!(summary.tokens_compacted, 0);
        assert_eq!(summary.original_request, None);
        assert!(summary.tool_calls.is_empty());
        assert_eq!(summary.time_span, Duration::ZERO);
        assert_eq!(summary.notes, None);
    }

    #[tokio::test]
    async fn mechanical_notes_always_none() {
        let entries = [full_entry(EntryPayload::Message(ChatMessage::user_text(
            "hello",
        )))];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.notes, None);
    }

    #[tokio::test]
    async fn mechanical_tokens_compacted_is_nonzero() {
        let entries = [full_entry(EntryPayload::Message(ChatMessage::user_text(
            "this is a reasonably long user message for token estimation",
        )))];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert!(
            summary.tokens_compacted > 0,
            "should estimate some tokens for non-empty entries"
        );
    }

    #[tokio::test]
    async fn mechanical_is_deterministic() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("fix it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"main.rs"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let s1 = strategy.compact(&refs).await.unwrap();
        let s2 = strategy.compact(&refs).await.unwrap();

        // MechanicalCompactionStrategy is deterministic for fixed input
        assert_eq!(s1.original_request, s2.original_request);
        assert_eq!(s1.tool_calls, s2.tool_calls);
        assert_eq!(s1.tokens_compacted, s2.tokens_compacted);
        assert_eq!(s1.entry_count, s2.entry_count);
        assert_eq!(s1.time_span, s2.time_span);
        assert_eq!(s1.notes, s2.notes);
    }

    // ── Argument summarisation tests ──────────────────────────────────────

    #[test]
    fn summarise_simple_string_argument() {
        let args = r#"{"path":"src/main.rs"}"#;
        let summary = summarise_arguments(args);
        assert!(summary.contains("path=src/main.rs"));
    }

    #[test]
    fn summarise_multiple_arguments() {
        let args = r#"{"path":"a.rs","offset":10}"#;
        let summary = summarise_arguments(args);
        assert!(summary.contains("path=a.rs"));
        assert!(summary.contains("offset=10"));
    }

    #[test]
    fn summarise_empty_json() {
        let args = "{}";
        let summary = summarise_arguments(args);
        // Empty object produces an empty string (no key-value pairs)
        assert!(
            summary.is_empty(),
            "empty JSON object should produce empty summary, got: {summary}"
        );
    }

    #[test]
    fn summarise_non_json_falls_back_gracefully() {
        let args = "not json at all";
        let summary = summarise_arguments(args);
        // Should not panic; just returns a truncated version
        assert!(!summary.is_empty());
    }

    #[test]
    fn summarise_truncates_long_values() {
        let long_val = "x".repeat(100);
        let args = format!(r#"{{"data":"{long_val}"}}"#);
        let summary = summarise_arguments(&args);
        // Should be truncated, not 100+ chars
        assert!(summary.len() < 80);
    }

    // ── Entry token estimation tests ──────────────────────────────────────

    #[test]
    fn estimate_tokens_for_user_message() {
        let entry = full_entry(EntryPayload::Message(ChatMessage::user_text("hello world")));
        let tokens = estimate_entry_tokens(&entry);
        assert!(tokens > 0, "should estimate tokens for a user message");
    }

    #[test]
    fn estimate_tokens_for_custom_entry() {
        let entry = full_entry(EntryPayload::Custom {
            kind: "rho.diagnostics.v1".to_owned(),
            data: serde_json::json!({"errors": 3}),
        });
        let tokens = estimate_entry_tokens(&entry);
        assert!(tokens > 0, "should estimate tokens for a custom entry");
    }
}
