// Append operations — all methods that create new entries in the session tree.
//
// Every public append delegates to `append_entry`, the private core that
// creates the `Entry`, links it to the current leaf, updates the leaf pointer,
// records the ID in append order, and auto-flushes to disk.
//
// LeafMoved entries are written only when the leaf moves to a non-adjacent
// position (during branch operations, not normal append).

use super::{Entry, EntryId, EntryPayload, EntryResolution, Session};
use crate::message::ChatMessage;
use crate::newtypes::ToolCallId;
use crate::tool::{ToolResult, ToolResultDetails};
use std::time::SystemTime;
use tracing::warn;

impl Session {
    // ── Public append operations ────────────────────────────────────────

    /// Append a user text message.
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_user_message(&mut self, text: &str) -> EntryId {
        self.append_entry(
            EntryPayload::Message(ChatMessage::user_text(text)),
            EntryResolution::Full,
        )
    }

    /// Append an assistant message (used by `send_current` to persist model responses).
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub(crate) fn append_assistant_message(&mut self, msg: ChatMessage) -> EntryId {
        self.append_entry(EntryPayload::Message(msg), EntryResolution::Full)
    }

    /// Append a tool result, applying secret redaction and bounded resolution.
    ///
    /// This is the fix for the shipping amnesia bug (P2.5-9). If the redacted
    /// content would consume more than half the prompt budget (per the calibrated
    /// estimator), it is truncated at a UTF-8-safe character boundary. The
    /// truncated content goes into the entry's `Message` payload; the *full*
    /// content is preserved in the session's details store and can be retrieved
    /// later via [`get_full_result`](Session::get_full_result).
    ///
    /// A `warn!` log is emitted when truncation occurs.
    ///
    /// Returns the [`EntryId`] of the new entry and the [`ToolResultDetails`]
    /// indicating whether truncation occurred.
    pub fn append_tool_result(
        &mut self,
        call_id: ToolCallId,
        result: &ToolResult,
    ) -> (EntryId, ToolResultDetails) {
        // Step 1: redact secrets
        let redacted = self.redactor.redact(&result.output);

        // Step 2: check if truncation is needed
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let max_tokens = (self.token_budget.prompt_budget() as f32
            * super::truncation::MAX_TOOL_RESULT_FRACTION) as usize;
        let estimated_tokens = self.estimator.estimate(&self.model, &redacted);

        let (content, details) = if estimated_tokens > max_tokens {
            // Truncate at a UTF-8-safe boundary
            let max_chars = super::truncation::chars_to_fit_tokens(
                &redacted,
                max_tokens,
                &self.model,
                self.estimator.as_ref(),
            );
            let original_size = redacted.len();
            let truncated = format!(
                "{}\n\n{}",
                &redacted[..super::truncation::floor_char_boundary(&redacted, max_chars)],
                super::truncation::truncation_footer(original_size),
            );

            warn!(
                original_size,
                truncated_size = truncated.len(),
                estimated_tokens,
                max_tokens,
                "tool result truncated to fit budget"
            );

            (
                truncated,
                ToolResultDetails::FullOutput {
                    original_size,
                    content: redacted,
                },
            )
        } else {
            (redacted, ToolResultDetails::None)
        };

        let id = self.append_entry(
            EntryPayload::Message(ChatMessage::tool_result(call_id, content)),
            EntryResolution::Full,
        );

        // Store structured details in the details store for later retrieval.
        // Both FullOutput (truncated results) and Diagnostics (compiler output)
        // are preserved — only None is skipped.
        if !matches!(details, ToolResultDetails::None) {
            self.details_store.insert(id.clone(), details.clone());
        }

        (id, details)
    }

    /// Append a compaction entry.
    ///
    /// `first_kept` identifies the first entry *after* the compacted range.
    /// `tokens_before` records the total estimated tokens of the compacted entries.
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_compaction(
        &mut self,
        summary: super::CompactionSummary,
        first_kept: EntryId,
        tokens_before: usize,
    ) -> EntryId {
        self.append_entry(
            EntryPayload::Compaction {
                summary,
                first_kept,
                tokens_before,
            },
            EntryResolution::Full,
        )
    }

    /// Append a branch summary entry.
    ///
    /// `from_id` identifies the leaf position that was abandoned.
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_branch_summary(
        &mut self,
        summary: super::CompactionSummary,
        from_id: EntryId,
    ) -> EntryId {
        self.append_entry(
            EntryPayload::BranchSummary { summary, from_id },
            EntryResolution::Full,
        )
    }

    /// Append a label on a target entry.
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_label(&mut self, target_id: EntryId, label: Option<String>) -> EntryId {
        self.append_entry(
            EntryPayload::Label { target_id, label },
            EntryResolution::Attached,
        )
    }

    /// Append a custom state entry (extension data that does NOT participate
    /// in the LLM context).
    ///
    /// The `kind` string follows the `ExtensionEntry::KIND` convention:
    /// `"<author>.<feature>.v<n>"` (e.g., `"rho.diagnostics.v1"`).
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_custom_state(&mut self, kind: String, data: serde_json::Value) -> EntryId {
        self.append_entry(
            EntryPayload::Custom { kind, data },
            EntryResolution::Attached,
        )
    }

    /// Append a custom message entry (extension content that DOES participate
    /// in the LLM context).
    ///
    /// The `kind` string follows the `ExtensionEntry::KIND` convention.
    ///
    /// Returns the [`EntryId`] of the new entry.
    pub fn append_custom_message(
        &mut self,
        kind: String,
        content: Vec<crate::message::ContentBlock>,
    ) -> EntryId {
        self.append_entry(
            EntryPayload::CustomMessage { kind, content },
            EntryResolution::Full,
        )
    }

    /// Append a `SessionEnded` entry marking the clean close of this session.
    ///
    /// The `reason` is a human-readable string explaining why the session
    /// ended (e.g. `"user quit"`, `"prompt file completed"`). The entry
    /// is `Attached` resolution — it does not participate in the LLM context.
    ///
    /// This is called on graceful exit. The absence of a `SessionEnded`
    /// entry in the JSONL file indicates an unclean shutdown (crash, kill,
    /// or Ctrl-C).
    pub fn close(&mut self, reason: impl Into<String>) {
        self.append_entry(
            EntryPayload::SessionEnded {
                reason: reason.into(),
            },
            EntryResolution::Attached,
        );
    }

    // ── Typed extension entry methods ─────────────────────────────────────

    /// Write a typed extension state entry using the [`ExtensionEntry`](super::extensions::ExtensionEntry) trait.
    ///
    /// The entry is stored as [`Custom`](EntryPayload::Custom) with
    /// `kind = E::KIND` and `data` serialized from `entry`. The resolution
    /// is [`Attached`](EntryResolution::Attached) — the content does not
    /// participate in the LLM context.
    pub fn write_custom_state<E: super::ExtensionEntry>(&mut self, entry: &E) -> EntryId {
        let data = serde_json::to_value(entry).unwrap_or_else(|e| {
            warn!(
                kind = E::KIND,
                error = %e,
                "failed to serialize ExtensionEntry, storing null"
            );
            serde_json::Value::Null
        });
        self.append_entry(
            EntryPayload::Custom {
                kind: E::KIND.to_owned(),
                data,
            },
            EntryResolution::Attached,
        )
    }

    /// Read a typed extension state entry back from the session tree.
    ///
    /// Looks up the entry at `id`, verifies that its `kind` matches
    /// `E::KIND`, and deserializes the `data` field into `E`.
    pub fn read_custom_state<E: super::ExtensionEntry>(&self, id: &EntryId) -> Option<E> {
        let entry = self.entries.get(id)?;
        match &entry.payload {
            EntryPayload::Custom { kind, data } if kind == E::KIND => {
                serde_json::from_value(data.clone()).ok()
            }
            _ => None,
        }
    }

    /// Write a typed extension message entry using the [`ExtensionMessageEntry`](super::extensions::ExtensionMessageEntry) trait.
    ///
    /// The entry is stored as [`CustomMessage`](EntryPayload::CustomMessage)
    /// with `kind = E::KIND` and `content` derived from `entry`. The resolution
    /// is [`Full`](EntryResolution::Full) — the content participates in the
    /// LLM context.
    pub fn write_custom_message<E: super::ExtensionMessageEntry>(&mut self, entry: &E) -> EntryId {
        let content = entry.content_blocks();
        self.append_entry(
            EntryPayload::CustomMessage {
                kind: E::KIND.to_owned(),
                content,
            },
            EntryResolution::Full,
        )
    }

    /// Read a typed extension message entry back from the session tree.
    ///
    /// Looks up the entry at `id`, verifies that its `kind` matches
    /// `E::KIND`, and reconstructs the typed value from the content blocks.
    pub fn read_custom_message<E: super::ExtensionMessageEntry>(&self, id: &EntryId) -> Option<E> {
        let entry = self.entries.get(id)?;
        match &entry.payload {
            EntryPayload::CustomMessage { kind, content } if kind == E::KIND => {
                E::from_content_blocks(content)
            }
            _ => None,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Core append: creates an entry, links it to the current leaf, updates
    /// the leaf pointer, records the ID in append order, and auto-flushes.
    ///
    /// Auto-flush writes the new entry to the JSONL file immediately. If the
    /// flush fails, a `warn!` is logged but the in-memory session is unaffected
    /// — the entry is still in the tree. The file will be caught up on the next
    /// successful flush or when the session is opened again.
    fn append_entry(&mut self, payload: EntryPayload, resolution: EntryResolution) -> EntryId {
        let id = EntryId::new();
        let entry = Entry {
            id: id.clone(),
            parent_id: self.leaf.clone(),
            timestamp: SystemTime::now(),
            resolution,
            payload,
        };
        self.entries.insert(id.clone(), entry);
        self.append_order.push(id.clone());
        self.leaf = Some(id.clone());

        // Auto-flush: write the new entry to disk immediately.
        // A crashed process loses at most one in-flight entry.
        if let Err(e) = self.flush() {
            warn!(
                error = %e,
                entry_id = %id,
                "auto-flush failed, entry is in memory but not on disk"
            );
        }

        id
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    use super::*;
    use crate::context::TokenBudget;
    use crate::message::ContentBlock;
    use crate::newtypes::ToolCallId;
    use crate::tool::ToolResult;
    use std::collections::BTreeMap;

    // ── Append operation tests ──────────────────────────────────────────

    #[test]
    fn append_user_message_creates_entry_linked_to_leaf() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");

        let entry = session.entry(&user_id).unwrap();
        assert_eq!(entry.parent_id, Some(root_id));
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(
            entry.payload,
            EntryPayload::Message(ChatMessage::User { .. })
        ));
        assert_eq!(session.leaf(), Some(user_id));
    }

    #[test]
    fn append_user_message_without_system_prompt() {
        let mut session = Session::in_memory("m", None, vec![], "/tmp");
        assert!(session.leaf().is_none());

        let user_id = session.append_user_message("hello");
        let entry = session.entry(&user_id).unwrap();
        assert!(entry.parent_id.is_none(), "first entry has no parent");
        assert_eq!(session.leaf(), Some(user_id));
    }

    #[test]
    fn append_assistant_message_links_to_previous() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi there"));

        let entry = session.entry(&asst_id).unwrap();
        assert_eq!(entry.parent_id, Some(user_id));
        assert_eq!(session.leaf(), Some(asst_id));
    }

    #[test]
    fn multiple_appends_form_chain() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root = session.leaf().unwrap();

        let id1 = session.append_user_message("msg1");
        let id2 = session.append_assistant_message(ChatMessage::assistant_text("reply1"));
        let id3 = session.append_user_message("msg2");

        assert_eq!(session.entry(&id1).unwrap().parent_id, Some(root));
        assert_eq!(session.entry(&id2).unwrap().parent_id, Some(id1));
        assert_eq!(session.entry(&id3).unwrap().parent_id, Some(id2));
        assert_eq!(session.leaf(), Some(id3));
    }

    // ── Bounded tool-result tests (amnesia bug fix) ─────────────────────

    #[test]
    fn tool_result_that_fits_is_not_truncated() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        let result = ToolResult::success("small output");
        let (id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

        let entry = session.entry(&id).unwrap();
        if let EntryPayload::Message(ChatMessage::Tool { content, .. }) = &entry.payload {
            let text = &content[0];
            let ContentBlock::Text { text } = text;
            assert_eq!(text, "small output");
        } else {
            panic!("expected Tool message");
        }
        assert_eq!(
            details,
            ToolResultDetails::None,
            "no truncation should occur"
        );
    }

    #[test]
    fn oversized_tool_result_is_truncated_with_full_output_preserved() {
        // Use a tiny budget so even moderate output triggers truncation
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10)); // prompt_budget = 90, half = 45 tokens

        // 2000 chars at 2.5 chars/token ≈ 800 tokens — well over the 45 token limit
        let huge_content = "x".repeat(2000);
        let result = ToolResult::success(&huge_content);

        let (id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

        let entry = session.entry(&id).unwrap();
        if let EntryPayload::Message(ChatMessage::Tool { content, .. }) = &entry.payload {
            let text = &content[0];
            let ContentBlock::Text { text } = text;
            // The truncated text must be shorter than the original
            assert!(
                text.len() < huge_content.len(),
                "truncated text should be shorter than original"
            );
            // The truncation footer must be present
            assert!(
                text.contains("[truncated"),
                "truncated text should contain the footer, got: {}",
                &text[text.len().saturating_sub(100)..]
            );
        } else {
            panic!("expected Tool message");
        }

        // The full output should be preserved in details
        if let ToolResultDetails::FullOutput {
            original_size,
            content,
        } = &details
        {
            assert_eq!(*original_size, huge_content.len());
            assert_eq!(content.len(), huge_content.len());
            assert_eq!(content, &huge_content);
        } else {
            panic!("expected FullOutput details, got {details:?}");
        }
    }

    #[test]
    fn truncation_point_is_utf8_safe() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10));

        // Build a string with multi-byte characters
        // Each '日' is 3 bytes in UTF-8
        let content = "日".repeat(500); // 1500 bytes
        let result = ToolResult::success(&content);

        let (id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

        // The entry should be created without panicking (valid UTF-8 slice)
        let entry = session.entry(&id).unwrap();
        if let EntryPayload::Message(ChatMessage::Tool {
            content: blocks, ..
        }) = &entry.payload
        {
            let ContentBlock::Text { text } = &blocks[0];
            // Must be valid UTF-8 (by construction — Rust ensures this)
            assert!(
                text.contains('日'),
                "truncated text should contain some 日 chars"
            );
        }

        // Full output preserved
        assert!(matches!(details, ToolResultDetails::FullOutput { .. }));
    }

    #[test]
    fn append_compaction_creates_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");

        let summary = super::super::CompactionSummary {
            original_request: Some("hello".to_owned()),
            current_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 2,
            time_span: std::time::Duration::from_secs(30),
            notes: None,
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
        };

        let id = session.append_compaction(summary, user_id, 200);
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(entry.payload, EntryPayload::Compaction { .. }));
        assert_eq!(session.leaf(), Some(id));
    }

    #[test]
    fn append_branch_summary_creates_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let from_id = session.append_user_message("hello");

        let summary = super::super::CompactionSummary {
            original_request: None,
            current_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(10),
            notes: None,
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
        };

        let id = session.append_branch_summary(summary, from_id);
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(entry.payload, EntryPayload::BranchSummary { .. }));
    }

    #[test]
    fn append_label_creates_attached_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let target = session.append_user_message("hello");

        let id = session.append_label(target, Some("checkpoint".to_owned()));
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Attached));
        assert!(matches!(entry.payload, EntryPayload::Label { .. }));
    }

    #[test]
    fn append_custom_state_creates_attached_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        let id = session.append_custom_state(
            "rho.diagnostics.v1".to_owned(),
            serde_json::json!({"errors": 3}),
        );
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Attached));
        if let EntryPayload::Custom { kind, data } = &entry.payload {
            assert_eq!(kind, "rho.diagnostics.v1");
            assert_eq!(data["errors"], 3);
        } else {
            panic!("expected Custom payload");
        }
    }

    #[test]
    fn append_custom_message_creates_full_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        let id = session.append_custom_message(
            "rho.diagnostics.v1".to_owned(),
            vec![ContentBlock::Text {
                text: "3 errors found".to_owned(),
            }],
        );
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        if let EntryPayload::CustomMessage { kind, content } = &entry.payload {
            assert_eq!(kind, "rho.diagnostics.v1");
            assert_eq!(content.len(), 1);
        } else {
            panic!("expected CustomMessage payload");
        }
    }
}
