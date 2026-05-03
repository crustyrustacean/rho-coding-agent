//! Session tree — adaptive-resolution conversation model.
//!
//! Replaces the flat `Vec<ChatMessage>` with a parent-linked tree of typed
//! [`Entry`] nodes, each carrying an explicit [`EntryResolution`] level. The
//! session's *leaf pointer* identifies the current position in the tree; the
//! context sent to the model is the leaf-to-root path, filtered by resolution.
//!
//! # Leaf-pointer contract
//!
//! The leaf pointer (`Session::leaf`) always references the most recently
//! appended entry during normal operation. It moves to a non-adjacent position
//! only during branch operations (e.g., `branch_to`), at which point a
//! [`LeafMoved`](EntryPayload::LeafMoved) entry is recorded for audit purposes.
//!
//! The leaf is never `None` after construction — `Session::new` creates a root
//! entry (the system message) and sets the leaf to its ID.
//!
//! # Module layout
//!
//! | Submodule | Contents |
//! |---|---|
//! | [`entry`] | [`Entry`], [`EntryPayload`], [`EntryResolution`], [`CompactionSummary`] |
//! | [`estimator`] | [`TokenEstimator`] trait, [`HeuristicEstimator`] |

pub mod entry;
pub mod estimator;

pub use entry::{CompactionSummary, Entry, EntryPayload, EntryResolution};
pub use estimator::{HeuristicEstimator, TokenEstimator};

use crate::context::{ContextManager, SlidingWindowContextManager, TokenBudget};
use crate::message::ChatMessage;
use crate::newtypes::{EntryId, SessionId, ToolCallId};
use crate::redact::Redactor;
use crate::schema::ToolSchema;
use crate::tool::{ToolResult, ToolResultDetails};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;
use tracing::warn;

// ── SessionHeader ─────────────────────────────────────────────────────────────

/// Metadata about a session's identity and origin.
///
/// `version` starts at 1 and will be incremented if the on-disk format changes
/// in a way that requires migration. `parent_session` links to a prior session
/// file if this session was forked from one (Phase 5+).
#[derive(Clone, Debug)]
pub struct SessionHeader {
    /// Unique identifier for this session.
    pub id: SessionId,
    /// On-disk format version (starts at 1).
    pub version: u32,
    /// When this session was created.
    pub created_at: SystemTime,
    /// The working directory the session was started in.
    pub cwd: PathBuf,
    /// Path to a parent session file, if this session was forked.
    pub parent_session: Option<PathBuf>,
}

// ── Session ───────────────────────────────────────────────────────────────────

/// A tree-shaped conversation session with adaptive resolution.
///
/// Each entry in the tree carries an [`EntryResolution`] that determines whether
/// it participates in the model's context. The *leaf pointer* identifies the
/// current position; the model sees the leaf-to-root path filtered by resolution.
///
/// # Construction
///
/// `Session::new` creates a root entry containing the system prompt and sets
/// the leaf to that entry. The leaf is never `None` after construction.
///
/// # Builder methods
///
/// Follow the same pattern as [`Conversation`](crate::Conversation):
/// `with_context_manager`, `with_token_budget`, `with_redactor`, plus the new
/// `with_estimator`.
///
/// # Persistence
///
/// Sessions persist to JSONL (Task 11). The in-memory representation is the
/// authoritative state; the on-disk log is an append-only record of every entry.
pub struct Session {
    /// Session identity and origin metadata.
    header: SessionHeader,
    /// All entries in the session tree, indexed by ID.
    entries: HashMap<EntryId, Entry>,
    /// The current leaf position. Always `Some` after construction.
    leaf: Option<EntryId>,
    /// Token estimator for budget-aware decisions.
    estimator: Box<dyn TokenEstimator>,
    /// Model identifier.
    model: String,
    /// Tool schemas sent with every request.
    #[allow(dead_code)]
    tools: Vec<ToolSchema>,
    /// Context window manager applied before each request.
    context_manager: Box<dyn ContextManager>,
    /// Token budget for the context manager.
    token_budget: TokenBudget,
    /// Secret redactor applied to tool results before they enter history.
    redactor: Redactor,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("header", &self.header)
            .field("entry_count", &self.entries.len())
            .field("leaf", &self.leaf)
            .field("model", &self.model)
            .field("token_budget", &self.token_budget)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Create a new session.
    ///
    /// The system prompt (if provided) becomes the first [`Entry`] with
    /// `parent_id = None` and `resolution: Full`. The leaf pointer is set to
    /// this root entry.
    pub fn new(
        model: impl Into<String>,
        system_prompt: Option<&str>,
        tools: Vec<ToolSchema>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let mut entries = HashMap::new();
        let leaf = if let Some(prompt) = system_prompt {
            let root = Entry {
                id: EntryId::new(),
                parent_id: None,
                timestamp: SystemTime::now(),
                resolution: EntryResolution::Full,
                payload: EntryPayload::Message(ChatMessage::system_text(prompt)),
            };
            let id = root.id.clone();
            entries.insert(id.clone(), root);
            Some(id)
        } else {
            None
        };

        Self {
            header: SessionHeader {
                id: SessionId::new(),
                version: 1,
                created_at: SystemTime::now(),
                cwd: cwd.into(),
                parent_session: None,
            },
            entries,
            leaf,
            estimator: Box::new(HeuristicEstimator::new()),
            model: model.into(),
            tools,
            context_manager: Box::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
        }
    }

    // ── Builder methods ───────────────────────────────────────────────────

    /// Override the context manager.
    #[must_use]
    pub fn with_context_manager(mut self, cm: Box<dyn ContextManager>) -> Self {
        self.context_manager = cm;
        self
    }

    /// Override the token budget.
    #[must_use]
    pub fn with_token_budget(mut self, budget: TokenBudget) -> Self {
        self.token_budget = budget;
        self
    }

    /// Override the secret redactor.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    /// Override the token estimator.
    #[must_use]
    pub fn with_estimator(mut self, estimator: Box<dyn TokenEstimator>) -> Self {
        self.estimator = estimator;
        self
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    /// The session header (identity, version, creation time, cwd).
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// The current leaf entry ID. `None` only if the session was constructed
    /// without a system prompt (before the first append).
    pub fn leaf(&self) -> Option<EntryId> {
        self.leaf.clone()
    }

    /// Look up an entry by ID.
    pub fn entry(&self, id: &EntryId) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// The model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Switch to a different model.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    /// The token budget.
    pub fn token_budget(&self) -> TokenBudget {
        self.token_budget
    }

    /// Read-only access to the redactor.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Read-only access to the estimator.
    pub fn estimator(&self) -> &dyn TokenEstimator {
        self.estimator.as_ref()
    }

    /// Mutable access to the estimator (for calibration).
    pub fn estimator_mut(&mut self) -> &mut dyn TokenEstimator {
        self.estimator.as_mut()
    }

    /// Total number of entries in the tree.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    // ── Append operations ─────────────────────────────────────────────────
    //
    // All append operations share the same skeleton:
    // 1. Create a new Entry with a fresh EntryId
    // 2. Set parent_id = self.leaf
    // 3. Insert into entries
    // 4. Update self.leaf to the new entry's ID
    // 5. Return the new EntryId
    //
    // LeafMoved entries are written only when the leaf moves to a
    // non-adjacent position (during branch operations, not normal append).

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
    #[allow(dead_code)]
    pub(crate) fn append_assistant_message(&mut self, msg: ChatMessage) -> EntryId {
        self.append_entry(EntryPayload::Message(msg), EntryResolution::Full)
    }

    /// Append a tool result, applying secret redaction and bounded resolution.
    ///
    /// This is the fix for the shipping amnesia bug (P2.5-9). If the redacted
    /// content would consume more than half the prompt budget (per the calibrated
    /// estimator), it is truncated at a UTF-8-safe character boundary. The
    /// truncated content goes into the entry's `Message` payload; the *full*
    /// content is preserved out-of-band as `ToolResultDetails::FullOutput`.
    ///
    /// A `warn!` log is emitted when truncation occurs.
    ///
    /// Returns the [`EntryId`] of the new entry.
    #[allow(dead_code)]
    pub(crate) fn append_tool_result(
        &mut self,
        call_id: ToolCallId,
        result: &ToolResult,
    ) -> EntryId {
        // Step 1: redact secrets
        let redacted = self.redactor.redact(&result.output);

        // Step 2: check if truncation is needed
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let max_tokens =
            (self.token_budget.prompt_budget() as f32 * MAX_TOOL_RESULT_FRACTION) as usize;
        let estimated_tokens = self.estimator.estimate(&redacted);

        let (content, details) = if estimated_tokens > max_tokens {
            // Truncate at a UTF-8-safe boundary
            let max_chars = chars_to_fit_tokens(&redacted, max_tokens, self.estimator.as_ref());
            let original_size = redacted.len();
            let truncated = format!(
                "{}\n\n{}",
                &redacted[..floor_char_boundary(&redacted, max_chars)],
                truncation_footer(original_size),
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

        let _ = details; // Will be stored on ToolResult when Session drives tool execution
        self.append_entry(
            EntryPayload::Message(ChatMessage::tool_result(call_id, content)),
            EntryResolution::Full,
        )
    }

    /// Append a tool result with full `ToolResult` metadata (including `details`).
    ///
    /// This is the public entry point for adding tool results to the session
    /// (e.g., from integration tests). It always applies redaction and
    /// bounded resolution — there is no way to bypass either through this API.
    ///
    /// Returns the [`EntryId`] of the new entry and the `ToolResultDetails`
    /// (if truncation occurred, the full output is preserved here).
    pub fn append_tool_result_with_details(
        &mut self,
        call_id: ToolCallId,
        result: &ToolResult,
    ) -> (EntryId, ToolResultDetails) {
        let redacted = self.redactor.redact(&result.output);

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let max_tokens =
            (self.token_budget.prompt_budget() as f32 * MAX_TOOL_RESULT_FRACTION) as usize;
        let estimated_tokens = self.estimator.estimate(&redacted);

        let (content, details) = if estimated_tokens > max_tokens {
            let max_chars = chars_to_fit_tokens(&redacted, max_tokens, self.estimator.as_ref());
            let original_size = redacted.len();
            let truncated = format!(
                "{}\n\n{}",
                &redacted[..floor_char_boundary(&redacted, max_chars)],
                truncation_footer(original_size),
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
        summary: CompactionSummary,
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
        summary: CompactionSummary,
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

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Core append: creates an entry, links it to the current leaf, updates
    /// the leaf pointer, and returns the new entry's ID.
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
        self.leaf = Some(id.clone());
        id
    }
}

// ── Bounded tool-result helpers ───────────────────────────────────────────────

/// Maximum fraction of the prompt budget that a single tool result may consume.
const MAX_TOOL_RESULT_FRACTION: f32 = 0.5;

/// Truncation footer appended to truncated tool results.
fn truncation_footer(original_size: usize) -> String {
    format!(
        "... [truncated; original size: {original_size} bytes — re-read the source with offset to access more]."
    )
}

/// Find the largest character boundary index ≤ `max_chars` in `s`.
///
/// Rust string slicing requires char boundaries. This finds the highest
/// index ≤ `max_chars` that falls on a valid char boundary.
fn floor_char_boundary(s: &str, max_chars: usize) -> usize {
    if max_chars >= s.len() {
        return s.len();
    }
    // Walk backwards from max_chars until we find a char boundary.
    let mut i = max_chars;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Estimate how many characters correspond to `target_tokens` tokens,
/// using the given estimator.
///
/// Works by solving `target_tokens = chars / ratio` for `chars`,
/// then clamping to the actual string length.
fn chars_to_fit_tokens(s: &str, target_tokens: usize, estimator: &dyn TokenEstimator) -> usize {
    // Use the estimator's ratio: if a string of length L has T tokens,
    // then chars/token ≈ L/T, so chars ≈ target_tokens * (L/T).
    // But we can also just iterate: start from the full string and shrink.
    // For simplicity, use a proportional estimate.
    let total_tokens = estimator.estimate(s);
    if total_tokens == 0 {
        return s.len();
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let ratio = s.len() as f32 / total_tokens as f32;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let estimated_chars = (target_tokens as f32 * ratio) as usize;
    estimated_chars.min(s.len())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    use super::*;
    use crate::message::ContentBlock;
    use crate::tool::ToolResult;

    // ── Construction tests ──────────────────────────────────────────────

    #[test]
    fn new_session_with_system_prompt_has_root_entry() {
        let session = Session::new("test-model", Some("you are helpful"), vec![], "/tmp");
        let leaf = session
            .leaf()
            .expect("leaf should be set after construction");
        let entry = session.entry(&leaf).expect("root entry should exist");
        assert!(entry.parent_id.is_none(), "root entry has no parent");
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(
            entry.payload,
            EntryPayload::Message(ChatMessage::System { .. })
        ));
    }

    #[test]
    fn new_session_without_system_prompt_has_no_entries() {
        let session = Session::new("test-model", None, vec![], "/tmp");
        assert!(session.leaf().is_none());
        assert_eq!(session.entry_count(), 0);
    }

    #[test]
    fn builder_methods_override_defaults() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(4096));
        assert_eq!(session.token_budget().context_window, 4096);
    }

    #[test]
    fn header_has_correct_version() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        assert_eq!(session.header().version, 1);
    }

    #[test]
    fn header_cwd_matches_constructor() {
        let session = Session::new("m", Some("sys"), vec![], "/project");
        assert_eq!(session.header().cwd, PathBuf::from("/project"));
    }

    #[test]
    fn header_session_id_is_unique() {
        let s1 = Session::new("m", Some("sys"), vec![], "/tmp");
        let s2 = Session::new("m", Some("sys"), vec![], "/tmp");
        assert_ne!(s1.header().id, s2.header().id);
    }

    #[test]
    fn set_model_updates_model() {
        let mut session = Session::new("old-model", Some("sys"), vec![], "/tmp");
        session.set_model("new-model");
        assert_eq!(session.model(), "new-model");
    }

    #[test]
    fn estimator_default_is_heuristic() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        let tokens = session.estimator().estimate("hello");
        assert!(tokens > 0);
    }

    #[test]
    fn estimator_mut_allows_calibration() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.estimator_mut().calibrate("test-model", 10, 12);
    }

    // ── Append operation tests ──────────────────────────────────────────

    #[test]
    fn append_user_message_creates_entry_linked_to_leaf() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::new("m", None, vec![], "/tmp");
        assert!(session.leaf().is_none());

        let user_id = session.append_user_message("hello");
        let entry = session.entry(&user_id).unwrap();
        assert!(entry.parent_id.is_none(), "first entry has no parent");
        assert_eq!(session.leaf(), Some(user_id));
    }

    #[test]
    fn append_assistant_message_links_to_previous() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi there"));

        let entry = session.entry(&asst_id).unwrap();
        assert_eq!(entry.parent_id, Some(user_id));
        assert_eq!(session.leaf(), Some(asst_id));
    }

    #[test]
    fn multiple_appends_form_chain() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        let result = ToolResult::success("small output");
        let (id, details) =
            session.append_tool_result_with_details(ToolCallId::from("call_1"), &result);

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
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10)); // prompt_budget = 90, half = 45 tokens

        // 2000 chars at 2.5 chars/token ≈ 800 tokens — well over the 45 token limit
        let huge_content = "x".repeat(2000);
        let result = ToolResult::success(&huge_content);

        let (id, details) =
            session.append_tool_result_with_details(ToolCallId::from("call_1"), &result);

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
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10));

        // Build a string with multi-byte characters
        // Each '日' is 3 bytes in UTF-8
        let content = "日".repeat(500); // 1500 bytes
        let result = ToolResult::success(&content);

        let (id, details) =
            session.append_tool_result_with_details(ToolCallId::from("call_1"), &result);

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
    fn floor_char_boundary_on_ascii() {
        assert_eq!(floor_char_boundary("hello world", 5), 5);
        assert_eq!(floor_char_boundary("hello", 10), 5); // beyond end
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn floor_char_boundary_on_multibyte() {
        // '日' is 3 bytes. "日日日" is 9 bytes.
        let s = "日日日";
        assert_eq!(s.len(), 9);
        // index 4 is not a char boundary (日=0..3, 日=3..6, 日=6..9)
        assert_eq!(floor_char_boundary(s, 4), 3); // floor to previous boundary
        assert_eq!(floor_char_boundary(s, 6), 6); // exact boundary
        assert_eq!(floor_char_boundary(s, 8), 6); // floor to previous boundary
    }

    // ── Append other entry types ────────────────────────────────────────

    #[test]
    fn append_compaction_creates_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");

        let summary = CompactionSummary {
            original_request: Some("hello".to_owned()),
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 2,
            time_span: std::time::Duration::from_secs(30),
            notes: None,
        };

        let id = session.append_compaction(summary, user_id, 200);
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(entry.payload, EntryPayload::Compaction { .. }));
        assert_eq!(session.leaf(), Some(id));
    }

    #[test]
    fn append_branch_summary_creates_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let from_id = session.append_user_message("hello");

        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(10),
            notes: None,
        };

        let id = session.append_branch_summary(summary, from_id);
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        assert!(matches!(entry.payload, EntryPayload::BranchSummary { .. }));
    }

    #[test]
    fn append_label_creates_attached_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let target = session.append_user_message("hello");

        let id = session.append_label(target, Some("checkpoint".to_owned()));
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Attached));
        assert!(matches!(entry.payload, EntryPayload::Label { .. }));
    }

    #[test]
    fn append_custom_state_creates_attached_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

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
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

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

    // ── TokenBudget tests ──────────────────────────────────────────────

    #[test]
    fn token_budget_prompt_budget_subtracts_reserve() {
        let budget = TokenBudget::with_reserve(32_768, 4096);
        assert_eq!(budget.prompt_budget(), 28_672);
    }

    #[test]
    fn token_budget_default_has_32k_window_and_4k_reserve() {
        let budget = TokenBudget::default();
        assert_eq!(budget.context_window, 32_768);
        assert_eq!(budget.completion_reserve, 4096);
        assert_eq!(budget.prompt_budget(), 28_672);
    }

    #[test]
    fn token_budget_max_tokens_is_context_window() {
        let budget = TokenBudget::new(16_384);
        assert_eq!(budget.max_tokens(), 16_384);
        assert_eq!(budget.context_window, 16_384);
    }
}
