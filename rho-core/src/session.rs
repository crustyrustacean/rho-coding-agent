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
//! # Extension entries
//!
//! Extensions can attach typed state to the session tree by implementing the
//! [`ExtensionEntry`] trait. This provides type-safe read/write access to
//! structured data that either participates in the LLM context
//! ([`CustomMessage`](EntryPayload::CustomMessage)) or stays out-of-band
//! ([`Custom`](EntryPayload::Custom)).
//!
//! ## Versioning convention
//!
//! The [`ExtensionEntry::KIND`] constant follows the pattern
//! `"<author>.<feature>.v<n>"` (e.g., `"rho.diagnostics.v1"`). When the
//! schema changes, bump the version number. Consumers that expect an older
//! version will receive `None` from [`Session::read_custom_state`] or
//! [`Session::read_custom_message`], providing a clean break on schema skew
//! without panics or data corruption.
//!
//! ## Example
//!
//! ```
//! use rho_core::session::ExtensionEntry;
//! use serde::{Serialize, Deserialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct DiagnosticsState {
//!     error_count: usize,
//!     last_error: Option<String>,
//! }
//!
//! impl ExtensionEntry for DiagnosticsState {
//!     const KIND: &'static str = "rho.diagnostics.v1";
//! }
//!
//! // Write:
//! // let id = session.write_custom_state(&DiagnosticsState {
//! //     error_count: 3,
//! //     last_error: Some("mismatched types".into()),
//! // });
//!
//! // Read:
//! // let state: Option<DiagnosticsState> = session.read_custom_state(id);
//! ```
//!
//! # Module layout
//!
//! | Submodule | Contents |
//! |---|---|
//! | [`entry`] | [`Entry`], [`EntryPayload`], [`EntryResolution`], [`CompactionSummary`] |
//! | [`estimator`] | [`TokenEstimator`] trait, [`HeuristicEstimator`] |

pub mod compaction;
pub mod entry;
pub mod estimator;

pub use compaction::{CompactionStrategy, MechanicalCompactionStrategy};
pub use entry::{CompactionSummary, Entry, EntryPayload, EntryResolution};
pub use estimator::{HeuristicEstimator, TokenEstimator};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::context::{ContextManager, SlidingWindowContextManager, TokenBudget};
use crate::error::{Result, RhoError};
use crate::message::{ChatMessage, ContentBlock};
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

    // ── Tree navigation ──────────────────────────────────────────────────

    /// Walk from the current leaf back to the root, collecting entries.
    ///
    /// Returns entries in leaf-to-root order (newest first). If the leaf
    /// is `None`, returns an empty vec.
    ///
    /// # Panics
    ///
    /// Does not panic — but if the tree is corrupt (a `parent_id` references
    /// a non-existent entry), the walk stops at the last reachable entry
    /// and a `warn!` is emitted.
    pub fn path_to_root(&self) -> Vec<&Entry> {
        let Some(mut current_id) = self.leaf.clone() else {
            return Vec::new();
        };

        let mut path = Vec::new();
        let mut seen = std::collections::HashSet::new();

        loop {
            let Some(entry) = self.entries.get(&current_id) else {
                warn!(id = %current_id, "path_to_root: entry not found, tree may be corrupt");
                break;
            };

            if !seen.insert(entry.id.clone()) {
                warn!(id = %current_id, "path_to_root: cycle detected, stopping");
                break;
            }

            path.push(entry);

            match &entry.parent_id {
                Some(parent_id) => current_id = parent_id.clone(),
                None => break, // reached root
            }
        }

        path
    }

    /// Return the direct children of an entry, sorted by timestamp (oldest
    /// first).
    ///
    /// This scans all entries to find those whose `parent_id` matches the
    /// given `id`. The scan is O(n) where n is the total number of entries;
    /// for sessions with up to tens of thousands of entries this is fine.
    /// A reverse index can be added later if needed.
    pub fn children(&self, id: &EntryId) -> Vec<EntryId> {
        let mut child_ids: Vec<EntryId> = self
            .entries
            .values()
            .filter(|e| e.parent_id.as_ref() == Some(id))
            .map(|e| e.id.clone())
            .collect();

        // Sort by timestamp for deterministic ordering
        child_ids.sort_by(|a, b| {
            let ta = self.entries.get(a).map(|e| e.timestamp);
            let tb = self.entries.get(b).map(|e| e.timestamp);
            ta.cmp(&tb)
        });

        child_ids
    }

    /// Move the leaf pointer to an existing entry, recording a
    /// [`LeafMoved`](EntryPayload::LeafMoved) entry for audit.
    ///
    /// This is the core branching operation: after `branch_to(id)`, the
    /// leaf-to-root path passes through `id` instead of the previous leaf.
    /// The old branch remains in the tree and is accessible via
    /// [`entry()`](Session::entry) and [`children()`](Session::children),
    /// but is no longer on the active path.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError::EntryNotFound`] if `id` does not exist in the
    /// session tree.
    ///
    /// # No-op
    ///
    /// If `id` is already the current leaf, no `LeafMoved` entry is written
    /// and the method returns `Ok(())`.
    pub fn branch_to(&mut self, id: &EntryId) -> Result<()> {
        // Validate that the target exists
        if !self.entries.contains_key(id) {
            return Err(RhoError::EntryNotFound(id.to_string()));
        }

        // No-op if already at this leaf
        if self.leaf.as_ref() == Some(id) {
            return Ok(());
        }

        let old_leaf = self.leaf.clone();

        // Move the leaf
        self.leaf = Some(id.clone());

        // Write a LeafMoved audit entry (non-adjacent move)
        let moved_entry = Entry {
            id: EntryId::new(),
            parent_id: self.leaf.clone(),
            timestamp: SystemTime::now(),
            resolution: EntryResolution::Attached,
            payload: EntryPayload::LeafMoved {
                from: old_leaf,
                to: id.clone(),
            },
        };
        let moved_id = moved_entry.id.clone();
        self.entries.insert(moved_id.clone(), moved_entry);
        // The LeafMoved entry itself becomes the new leaf so subsequent
        // appends link from here.
        self.leaf = Some(moved_id);

        Ok(())
    }

    /// Move the leaf to an existing entry and append a
    /// [`BranchSummary`](EntryPayload::BranchSummary) at the new position.
    ///
    /// This combines [`branch_to`](Session::branch_to) with a summary of
    /// why the branch happened. The `from_id` identifies the leaf position
    /// that was abandoned; the `summary` describes what was on that branch.
    ///
    /// After this call, the leaf is at the newly-appended `BranchSummary`
    /// entry.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError::EntryNotFound`] if `id` does not exist.
    pub fn branch_with_summary(
        &mut self,
        id: &EntryId,
        summary: CompactionSummary,
        from_id: EntryId,
    ) -> Result<()> {
        self.branch_to(id)?;
        self.append_branch_summary(summary, from_id);
        Ok(())
    }

    // ── Compaction ────────────────────────────────────────────────────

    /// Compact the oldest entries whose total estimated tokens exceed
    /// `threshold`, using the given [`CompactionStrategy`].
    ///
    /// This is the core compaction operation. It:
    /// 1. Walks the leaf-to-root path and selects the oldest contiguous
    ///    entries whose cumulative estimated tokens exceed `threshold`.
    /// 2. Calls `strategy.compact()` on the selected entries to produce a
    ///    [`CompactionSummary`].
    /// 3. Appends a [`Compaction`](EntryPayload::Compaction) entry to the
    ///    tree.
    /// 4. Transitions the compacted entries' resolution from `Full` to
    ///    `Compacted { into }`, where `into` is the new Compaction entry's
    ///    ID.
    ///
    /// The compacted entries are **not deleted** — they remain in the tree
    /// at lower resolution, accessible via [`entry()`](Session::entry), but
    /// bypassed by [`path_messages()`](Session::path_messages) and
    /// [`fit_path`](ContextManager::fit_path).
    ///
    /// # Invariants
    ///
    /// - The system message (root entry) is never compacted.
    /// - The most recent entry (the leaf) is never compacted.
    /// - At least one entry remains un-compacted after this operation.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the compaction strategy fails.
    ///
    /// # Returns
    ///
    /// The [`EntryId`] of the new Compaction entry.
    pub async fn compact_older_than(
        &mut self,
        threshold: usize,
        strategy: &dyn compaction::CompactionStrategy,
    ) -> Result<EntryId> {
        // Walk the leaf-to-root path and reverse to get chronological order.
        let path = self.path_to_root();
        let chronological: Vec<&Entry> = path.into_iter().rev().collect();

        if chronological.len() <= 1 {
            // Nothing to compact (only the root, or empty)
            return Err(RhoError::Unexpected(anyhow::anyhow!(
                "cannot compact: session has too few entries"
            )));
        }

        // Find the contiguous range of oldest entries whose tokens exceed
        // the threshold. We never compact the root (index 0) or the leaf
        // (last entry).
        let mut cumulative_tokens: usize = 0;
        let mut compact_end: usize = 0; // exclusive upper bound; 0 means not reached

        // Start from index 1 (skip the root system message)
        for (i, entry) in chronological.iter().enumerate().skip(1) {
            // Don't compact the last entry (the leaf)
            if i == chronological.len() - 1 {
                break;
            }

            // Only compact Full-resolution entries
            if !matches!(entry.resolution, EntryResolution::Full) {
                continue;
            }

            cumulative_tokens +=
                estimate_entry_tokens_for_compaction(entry, self.estimator.as_ref());
            compact_end = i + 1;

            if cumulative_tokens >= threshold {
                break;
            }
        }

        // If we didn't accumulate enough tokens, there's nothing to compact.
        if cumulative_tokens < threshold || compact_end <= 1 {
            return Err(RhoError::Unexpected(anyhow::anyhow!(
                "cannot compact: not enough full-resolution entries exceeding threshold"
            )));
        }

        // Collect the entries to compact (indices 1..compact_end)
        let to_compact: Vec<&Entry> = chronological[1..compact_end].to_vec();
        if to_compact.is_empty() {
            return Err(RhoError::Unexpected(anyhow::anyhow!(
                "cannot compact: no entries selected"
            )));
        }

        // Collect the IDs of entries to compact BEFORE any mutation
        let compacted_ids: Vec<EntryId> = to_compact.iter().map(|e| e.id.clone()).collect();
        let first_kept_id = chronological[compact_end].id.clone();
        let tokens_before = cumulative_tokens;

        // Generate the summary
        let summary = strategy.compact(&to_compact).await?;

        // Append the Compaction entry
        let compaction_id = self.append_compaction(summary, first_kept_id, tokens_before);

        // Transition the compacted entries' resolution to Compacted
        for entry_id in &compacted_ids {
            if let Some(entry) = self.entries.get_mut(entry_id) {
                entry.resolution = EntryResolution::Compacted {
                    into: compaction_id.clone(),
                };
            }
        }

        Ok(compaction_id)
    }

    // ── Context building ────────────────────────────────────────────────

    /// Build the message list for the model from the leaf-to-root path.
    ///
    /// This is the `Session` equivalent of `Conversation::messages()` —
    /// it returns the messages that would be sent to the model, after
    /// applying resolution filtering and overhead subtraction via
    /// [`ContextManager::fit_path`].
    ///
    /// Returns the fitted messages in chronological order (system first),
    /// ready to be placed in a [`ChatRequest`](crate::request::ChatRequest).
    pub fn path_messages(&self) -> Vec<ChatMessage> {
        // Walk leaf-to-root, then reverse to get chronological order.
        let path = self.path_to_root();
        let entries: Vec<&Entry> = path.into_iter().rev().collect();

        self.context_manager.fit_path(
            &entries,
            self.token_budget,
            self.estimator.as_ref(),
            &self.tools,
        )
    }

    /// Send the current session state to the model and persist the response.
    ///
    /// This is the `Session` equivalent of `Conversation::send_current`:
    /// 1. Builds the message list via [`path_messages`](Self::path_messages).
    /// 2. Constructs a [`ChatRequest`](crate::request::ChatRequest).
    /// 3. Calls the client.
    /// 4. Persists the assistant response as an appended entry.
    /// 5. Calibrates the estimator against the actual `prompt_tokens`.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`](crate::error::RhoError) if the HTTP request fails
    /// or the response cannot be parsed.
    pub async fn send_current(
        &mut self,
        client: &dyn crate::client::ChatClient,
    ) -> crate::error::Result<crate::conversation::AssistantResponse> {
        use crate::request::ChatRequest;
        use crate::response::FinishReason;

        let fitted = self.path_messages();

        // Estimate tokens for the request before sending (for calibration).
        let estimated_tokens = Self::estimate_messages_tokens(&fitted);

        let request = ChatRequest {
            model: self.model.clone(),
            messages: fitted,
            tools: self.tools.clone(),
        };

        let response = client.chat(request).await?;
        let choice = &response.choices[0];

        // Calibrate estimator if the API returned prompt_tokens.
        let usage = &response.usage;
        if usage.prompt_tokens > 0 {
            self.estimator
                .calibrate(&self.model, estimated_tokens, usage.prompt_tokens);
        }

        if let FinishReason::ToolCalls = choice.finish_reason {
            let tool_calls = choice.message.tool_calls.clone();
            // Persist assistant message with tool_calls BEFORE returning.
            self.append_assistant_message(ChatMessage::Assistant {
                content: if choice.message.content.is_empty() {
                    vec![]
                } else {
                    vec![crate::message::ContentBlock::Text {
                        text: choice.message.content.clone(),
                    }]
                },
                tool_calls: tool_calls.clone(),
            });
            Ok(crate::conversation::AssistantResponse::ToolCalls(
                tool_calls,
            ))
        } else {
            let text = choice.message.content.clone();
            self.append_assistant_message(ChatMessage::assistant_text(&text));
            Ok(crate::conversation::AssistantResponse::Message(text))
        }
    }

    /// Estimate the total tokens for a slice of messages.
    fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
        use crate::context::approximate_tokens;
        // We use approximate_tokens for a consistent per-message estimate,
        // then scale by the estimator's model-specific ratio.
        // For now, approximate_tokens gives chars/4 which is close enough
        // for calibration. The estimator's calibrate() will correct the
        // ratio anyway.
        messages.iter().map(approximate_tokens).sum()
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

    // ── Typed extension entry methods ─────────────────────────────────────

    /// Write a typed extension state entry using the [`ExtensionEntry`] trait.
    ///
    /// The entry is stored as [`Custom`](EntryPayload::Custom) with
    /// `kind = E::KIND` and `data` serialized from `entry`. The resolution
    /// is [`Attached`](EntryResolution::Attached) — the content does not
    /// participate in the LLM context.
    ///
    /// # Example
    ///
    /// ```
    /// use rho_core::session::ExtensionEntry;
    /// use serde::{Serialize, Deserialize};
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct MyState { count: usize }
    ///
    /// impl ExtensionEntry for MyState {
    ///     const KIND: &'static str = "acme.counter.v1";
    /// }
    ///
    /// // session.write_custom_state(&MyState { count: 42 });
    /// ```
    ///
    /// See also [`read_custom_state`](Session::read_custom_state) for the
    /// typed read counterpart.
    pub fn write_custom_state<E: ExtensionEntry>(&mut self, entry: &E) -> EntryId {
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
    ///
    /// Returns `None` if:
    /// - The entry does not exist.
    /// - The entry's payload is not [`Custom`](EntryPayload::Custom).
    /// - The entry's `kind` does not match `E::KIND` (schema version skew).
    /// - Deserialization fails (corrupt or incompatible data).
    ///
    /// # Schema versioning
    ///
    /// Because `KIND` includes the version number, a consumer expecting
    /// `"acme.counter.v1"` will get `None` when reading an entry written
    /// as `"acme.counter.v2"`. This provides a clean break on schema
    /// changes without panics or silent data corruption.
    pub fn read_custom_state<E: ExtensionEntry>(&self, id: &EntryId) -> Option<E> {
        let entry = self.entries.get(id)?;
        match &entry.payload {
            EntryPayload::Custom { kind, data } if kind == E::KIND => {
                serde_json::from_value(data.clone()).ok()
            }
            _ => None,
        }
    }

    /// Write a typed extension message entry using the [`ExtensionEntry`] trait.
    ///
    /// The entry is stored as [`CustomMessage`](EntryPayload::CustomMessage)
    /// with `kind = E::KIND` and `content` derived from `entry`. The resolution
    /// is [`Full`](EntryResolution::Full) — the content participates in the
    /// LLM context.
    ///
    /// The content blocks are produced by calling `E::content_blocks()`. By
    /// default this serializes the entry to a JSON string and wraps it in a
    /// single `ContentBlock::Text`. Extensions that need richer formatting
    /// can override `content_blocks()` in their trait implementation.
    pub fn write_custom_message<E: ExtensionMessageEntry>(&mut self, entry: &E) -> EntryId {
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
    ///
    /// Returns `None` if:
    /// - The entry does not exist.
    /// - The entry's payload is not [`CustomMessage`](EntryPayload::CustomMessage).
    /// - The entry's `kind` does not match `E::KIND` (schema version skew).
    /// - Reconstruction via `E::from_content_blocks()` fails.
    pub fn read_custom_message<E: ExtensionMessageEntry>(&self, id: &EntryId) -> Option<E> {
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

// ── ExtensionEntry trait ─────────────────────────────────────────────────────

/// A trait for typed extension entries stored in the session tree.
///
/// Extensions implement this trait for their state types to get type-safe
/// read/write access to [`Custom`](EntryPayload::Custom) entries. The `KIND`
/// constant is stored alongside the serialized data and checked on read, so
/// schema version skew produces a clean `None` rather than a deserialization
/// error or silent corruption.
///
/// # Versioning convention
///
/// `KIND` should follow the pattern `"<author>.<feature>.v<n>"`, e.g.:
/// - `"rho.diagnostics.v1"`
/// - `"acme.linter.v2"`
///
/// When the schema changes incompatibly, bump the version number. Consumers
/// expecting the old version get `None` from [`Session::read_custom_state`].
///
/// # Example
///
/// ```
/// use rho_core::session::ExtensionEntry;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct DiagnosticsState {
///     error_count: usize,
/// }
///
/// impl ExtensionEntry for DiagnosticsState {
///     const KIND: &'static str = "rho.diagnostics.v1";
/// }
/// ```
pub trait ExtensionEntry: Serialize + DeserializeOwned + 'static {
    /// Namespaced kind identifier, including version.
    ///
    /// Follow the convention `"<author>.<feature>.v<n>"` to enable
    /// forward-compatible schema evolution.
    const KIND: &'static str;
}

/// A trait for typed extension *message* entries that participate in the LLM context.
///
/// This is the `CustomMessage` counterpart to [`ExtensionEntry`]. Extensions
/// implement this trait for content types that should be visible to the
/// model (resolution = Full), as opposed to [`ExtensionEntry`] which stores
/// state out-of-band (resolution = Attached).
///
/// The trait provides `content_blocks()` for writing and `from_content_blocks()`
/// for reading. The default `content_blocks()` serializes to a JSON string;
/// the default `from_content_blocks()` deserializes from the first text block.
/// Override both when you need richer formatting.
///
/// # Example
///
/// ```
/// use rho_core::session::ExtensionMessageEntry;
/// use rho_core::message::ContentBlock;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct LintSummary {
///     warnings: usize,
/// }
///
/// impl ExtensionMessageEntry for LintSummary {
///     const KIND: &'static str = "rho.lint-summary.v1";
/// }
/// ```
pub trait ExtensionMessageEntry: Serialize + DeserializeOwned + 'static {
    /// Namespaced kind identifier, including version.
    const KIND: &'static str;

    /// Serialize this entry into content blocks for storage.
    ///
    /// The default implementation serializes to a JSON string and wraps it
    /// in a single `ContentBlock::Text`. Override for richer formatting.
    fn content_blocks(&self) -> Vec<ContentBlock> {
        let json = serde_json::to_string(self).unwrap_or_default();
        vec![ContentBlock::Text { text: json }]
    }

    /// Reconstruct a typed value from content blocks.
    ///
    /// The default implementation reads the first `ContentBlock::Text` and
    /// deserializes it from JSON. Returns `None` if there are no text blocks
    /// or deserialization fails.
    fn from_content_blocks(blocks: &[ContentBlock]) -> Option<Self> {
        blocks.iter().find_map(|b| match b {
            ContentBlock::Text { text } => serde_json::from_str(text).ok(),
        })
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

/// Estimate the token count for an entry, using the calibrated estimator
/// for message content.
///
/// This is used by [`Session::compact_older_than`] to decide which entries
/// to compact. It differs from the compaction module's `estimate_entry_tokens`
/// by using the calibrated estimator rather than a fixed chars/4 heuristic,
/// giving more accurate budget decisions.
fn estimate_entry_tokens_for_compaction(entry: &Entry, estimator: &dyn TokenEstimator) -> usize {
    match &entry.payload {
        EntryPayload::Message(msg) => {
            let text = format_message_text(msg);
            estimator.estimate(&text).max(1)
        }
        EntryPayload::Compaction {
            summary,
            tokens_before,
            ..
        } => {
            // For existing compaction entries, use the recorded tokens_before
            // plus the summary's own token cost
            let summary_text = format_compaction_summary_text(summary);
            *tokens_before + estimator.estimate(&summary_text)
        }
        EntryPayload::BranchSummary { summary, .. } => {
            let text = format_compaction_summary_text(summary);
            estimator.estimate(&text).max(1)
        }
        EntryPayload::Custom { data, .. } => estimator.estimate(&data.to_string()).max(1),
        EntryPayload::CustomMessage { content, .. } => {
            let text: String = content
                .iter()
                .map(|b| match b {
                    crate::message::ContentBlock::Text { text } => text.as_str(),
                })
                .collect();
            estimator.estimate(&text).max(1)
        }
        EntryPayload::ModelChange { model } => estimator.estimate(model).max(1),
        EntryPayload::Label { label, .. } => {
            let text = label.as_deref().unwrap_or("");
            estimator.estimate(text).max(1)
        }
        EntryPayload::SessionInfo { name } => estimator.estimate(name).max(1),
        EntryPayload::LeafMoved { .. } => 1,
    }
}

/// Format a `ChatMessage` into a single string for token estimation.
///
/// This is a rough approximation — we just concatenate all text content.
/// The estimator's calibration corrects for structural overhead over time.
fn format_message_text(msg: &ChatMessage) -> String {
    use crate::message::ContentBlock;

    let mut text = String::new();
    match msg {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. } => {
            for block in content {
                let ContentBlock::Text { text: t } = block;
                text.push_str(t);
                text.push(' ');
            }
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            text.push_str(tool_call_id);
            text.push(' ');
            for block in content {
                let ContentBlock::Text { text: t } = block;
                text.push_str(t);
                text.push(' ');
            }
        }
    }
    text
}

/// Format a `CompactionSummary` into a single string for token estimation.
fn format_compaction_summary_text(summary: &CompactionSummary) -> String {
    let mut text = String::new();
    if let Some(ref req) = summary.original_request {
        text.push_str(req);
        text.push(' ');
    }
    for (name, calls) in &summary.tool_calls {
        text.push_str(name);
        for call in calls {
            text.push(' ');
            text.push_str(call);
        }
    }
    if let Some(ref notes) = summary.notes {
        text.push(' ');
        text.push_str(notes);
    }
    text
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
    use crate::newtypes::ToolName;
    use crate::tool::ToolResult;
    use serde::{Deserialize, Serialize};

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

    // ── Tree navigation tests ───────────────────────────────────────────

    #[test]
    fn path_to_root_returns_leaf_to_root_order() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let path = session.path_to_root();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id, asst_id, "first entry should be the leaf");
        assert_eq!(path[1].id, user_id, "second entry should be user message");
        assert_eq!(path[2].id, root_id, "last entry should be the root");
    }

    #[test]
    fn path_to_root_empty_when_no_leaf() {
        let session = Session::new("m", None, vec![], "/tmp");
        assert!(session.path_to_root().is_empty());
    }

    #[test]
    fn path_to_root_single_entry() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let path = session.path_to_root();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].id, root_id);
    }

    #[test]
    fn path_to_root_has_no_duplicates() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let id1 = session.append_user_message("msg1");
        let id2 = session.append_assistant_message(ChatMessage::assistant_text("reply1"));
        let id3 = session.append_user_message("msg2");

        let path = session.path_to_root();
        let ids: std::collections::HashSet<_> = path.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), path.len(), "path should have no duplicate IDs");

        // Verify exact order: leaf → ... → root
        assert_eq!(path[0].id, id3);
        assert_eq!(path[1].id, id2);
        assert_eq!(path[2].id, id1);
        assert_eq!(path[3].id, root_id);
    }

    #[test]
    fn children_returns_direct_children_sorted_by_timestamp() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();

        // Root has one child (the user message)
        let user_id = session.append_user_message("hello");
        let children = session.children(&root_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], user_id);
    }

    #[test]
    fn children_of_leaf_is_empty() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        // The leaf has no children yet (no appends after it)
        // But actually, the leaf IS the root, and we haven't appended anything
        // So it has 0 children
        assert!(session.children(&root_id).is_empty());

        // Now append something — the root has a child, the new leaf doesn't
        let user_id = session.append_user_message("hello");
        let new_leaf = session.leaf().unwrap();
        assert_eq!(session.children(&root_id).len(), 1);
        assert!(session.children(&new_leaf).is_empty());

        // user_id is both a child of root and a parent of nothing
        assert_eq!(session.children(&user_id).len(), 0);
    }

    #[test]
    fn children_of_forked_entry_includes_both_branches() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");

        // First branch: assistant reply
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to user_id and take a different path
        session.branch_to(&user_id).unwrap();
        let asst_id_2 = session.append_assistant_message(ChatMessage::assistant_text("reply B"));

        // user_id should have two children: the original assistant and the
        // LeafMoved entry (which then leads to reply B)
        let children = session.children(&user_id);
        assert_eq!(
            children.len(),
            2,
            "user_id should have 2 children (two branches)"
        );
        assert!(
            children.contains(&asst_id),
            "original assistant should be a child"
        );
        // The second child is the LeafMoved entry, whose child is asst_id_2
        let leaf_moved_id = children.iter().find(|id| **id != asst_id).unwrap();
        let leaf_moved_children = session.children(leaf_moved_id);
        assert!(leaf_moved_children.contains(&asst_id_2));
    }

    #[test]
    fn children_of_nonexistent_entry_is_empty() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        assert!(session.children(&fake_id).is_empty());
    }

    #[test]
    fn branch_to_moves_leaf() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply"));

        // Branch back to user_id
        session.branch_to(&user_id).unwrap();

        // Leaf should now be at a LeafMoved entry whose parent is user_id
        let leaf_id = session.leaf().unwrap();
        let leaf_entry = session.entry(&leaf_id).unwrap();
        assert_eq!(leaf_entry.parent_id, Some(user_id));
        assert!(matches!(leaf_entry.payload, EntryPayload::LeafMoved { .. }));
    }

    #[test]
    fn branch_to_writes_leaf_moved_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");

        // Branch to root
        session.branch_to(&root_id).unwrap();

        // Find the LeafMoved entry
        let leaf_id = session.leaf().unwrap();
        let leaf_entry = session.entry(&leaf_id).unwrap();
        if let EntryPayload::LeafMoved { from, to } = &leaf_entry.payload {
            assert_eq!(*from, Some(user_id), "from should be the old leaf");
            assert_eq!(*to, root_id, "to should be the target");
        } else {
            panic!("expected LeafMoved payload, got {:?}", leaf_entry.payload);
        }
        assert!(matches!(leaf_entry.resolution, EntryResolution::Attached));
    }

    #[test]
    fn branch_to_errors_on_nonexistent_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result = session.branch_to(&fake_id);
        assert!(result.is_err());
        if let Err(RhoError::EntryNotFound(id)) = &result {
            assert_eq!(id, &*fake_id);
        } else {
            panic!("expected EntryNotFound error, got {result:?}");
        }
    }

    #[test]
    fn branch_to_is_noop_when_already_at_target() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let leaf_id = session.leaf().unwrap();
        let entry_count_before = session.entry_count();

        session.branch_to(&leaf_id).unwrap();

        // No LeafMoved entry should be written
        assert_eq!(session.entry_count(), entry_count_before);
        assert_eq!(session.leaf(), Some(leaf_id));
    }

    #[test]
    fn branch_to_then_append_extends_from_new_position() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to user message and take a different path
        session.branch_to(&user_id).unwrap();
        let new_reply_id = session.append_assistant_message(ChatMessage::assistant_text("reply B"));

        // The new reply should be reachable from the leaf
        let path = session.path_to_root();
        let path_ids: Vec<_> = path.iter().map(|e| e.id.clone()).collect();
        assert!(
            path_ids.contains(&new_reply_id),
            "new reply should be on the leaf path"
        );
        assert!(
            path_ids.contains(&user_id),
            "user message should be on the leaf path"
        );
        assert!(
            path_ids.contains(&root_id),
            "root should be on the leaf path"
        );
    }

    #[test]
    fn branch_to_old_branch_still_in_tree() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_a_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back and take a different path
        session.branch_to(&user_id).unwrap();
        let _asst_b_id = session.append_assistant_message(ChatMessage::assistant_text("reply B"));

        // The old assistant message is still in the tree but NOT on the leaf path
        let old_entry = session.entry(&asst_a_id);
        assert!(old_entry.is_some(), "old branch entry should still exist");

        let path = session.path_to_root();
        let path_ids: Vec<_> = path.iter().map(|e| e.id.clone()).collect();
        assert!(
            !path_ids.contains(&asst_a_id),
            "old assistant should NOT be on the current leaf path"
        );
    }

    #[test]
    fn branch_with_summary_appends_summary_after_branch() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        let summary = CompactionSummary {
            original_request: Some("hello".to_owned()),
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(30),
            notes: None,
        };

        session
            .branch_with_summary(&root_id, summary, user_id.clone())
            .unwrap();

        // Leaf should now be at a BranchSummary entry
        let leaf_id = session.leaf().unwrap();
        let leaf_entry = session.entry(&leaf_id).unwrap();
        if let EntryPayload::BranchSummary {
            summary: s,
            from_id,
        } = &leaf_entry.payload
        {
            assert_eq!(*from_id, user_id, "from_id should reference abandoned leaf");
            assert_eq!(s.original_request, Some("hello".to_owned()));
        } else {
            panic!(
                "expected BranchSummary payload, got {:?}",
                leaf_entry.payload
            );
        }
    }

    #[test]
    fn branch_with_summary_errors_on_nonexistent_entry() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 0,
            entry_count: 0,
            time_span: std::time::Duration::ZERO,
            notes: None,
        };

        let result = session.branch_with_summary(&fake_id, summary, EntryId::new());
        assert!(result.is_err());
        if let Err(RhoError::EntryNotFound(id)) = &result {
            assert_eq!(id, &*fake_id);
        } else {
            panic!("expected EntryNotFound error, got {result:?}");
        }
    }

    #[test]
    fn path_to_root_after_branch_reflects_new_path() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_a_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to root
        session.branch_to(&root_id).unwrap();

        let path = session.path_to_root();
        let path_ids: Vec<_> = path.iter().map(|e| e.id.clone()).collect();

        // The path should go: LeafMoved → root
        // It should NOT include user_id or asst_a_id
        assert!(path_ids.contains(&root_id), "root should be on the path");
        assert!(
            !path_ids.contains(&user_id),
            "user_id should NOT be on the branched path"
        );
        assert!(
            !path_ids.contains(&asst_a_id),
            "old assistant should NOT be on the branched path"
        );
    }

    #[test]
    fn path_to_root_skips_attached_entries() {
        // This test verifies the structural behavior: Attached entries (like
        // LeafMoved) are in the tree but whether they appear in path_to_root
        // depends on the path traversal (they are on the path since the leaf
        // points to them). The *filtering* of Attached entries from the model
        // context is done by fit_path (Task 8), not path_to_root.
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let _user_id = session.append_user_message("hello");

        // Branch to root — creates a LeafMoved (Attached) entry
        session.branch_to(&root_id).unwrap();

        let path = session.path_to_root();
        // The LeafMoved entry IS in the path (it's the leaf)
        let leaf_entry = &path[0];
        assert!(matches!(leaf_entry.resolution, EntryResolution::Attached));
        assert!(matches!(leaf_entry.payload, EntryPayload::LeafMoved { .. }));
    }

    // ── Context building tests (Task 8) ────────────────────────────────

    #[test]
    fn path_messages_returns_chronological_messages() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));
        session.append_user_message("how are you?");

        let messages = session.path_messages();

        // Should be in chronological order: System, User, Assistant, User
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
        assert!(matches!(messages[2], ChatMessage::Assistant { .. }));
        assert!(matches!(messages[3], ChatMessage::User { .. }));
    }

    #[test]
    fn path_messages_filters_compacted_entries() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi"));

        // Manually mark the user entry as compacted
        if let Some(entry) = session.entries.get_mut(&user_id) {
            entry.resolution = EntryResolution::Compacted {
                into: EntryId::from("test"),
            };
        }

        let messages = session.path_messages();

        // The compacted user message should NOT appear
        let has_hello = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("hello")
                })
            } else {
                false
            }
        });
        assert!(!has_hello, "compacted entry should be filtered out");

        // The assistant message should still appear
        let has_hi = messages.iter().any(|m| {
            if let ChatMessage::Assistant {
                content,
                tool_calls,
            } = m
            {
                tool_calls.is_empty()
                    && content.iter().any(|b| {
                        let ContentBlock::Text { text } = b;
                        text.contains("hi")
                    })
            } else {
                false
            }
        });
        assert!(has_hi, "non-compacted entry should be present");
    }

    #[test]
    fn path_messages_filters_attached_entries() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Add an Attached entry (label)
        let leaf_id = session.leaf().unwrap();
        session.append_label(leaf_id, Some("checkpoint".to_owned()));

        let messages = session.path_messages();

        // Only System + User should appear; the Label (Attached) should be filtered
        assert_eq!(messages.len(), 2, "only System and User should appear");
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
    }

    #[test]
    fn path_messages_renders_compaction_summary() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("fix the bug");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));

        let summary = CompactionSummary {
            original_request: Some("fix the bug".to_owned()),
            tool_calls: {
                let mut map = std::collections::BTreeMap::new();
                map.insert(ToolName::from("read_file"), vec!["src/main.rs".to_owned()]);
                map
            },
            tokens_compacted: 1024,
            entry_count: 3,
            time_span: std::time::Duration::from_secs(45),
            notes: Some("compacted for budget".to_owned()),
        };

        let first_kept = session.leaf().unwrap();
        session.append_compaction(summary, first_kept, 2048);

        let messages = session.path_messages();

        // The compaction summary should be rendered as a synthetic User message
        let compaction_msg = messages.iter().find(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            compaction_msg.is_some(),
            "compaction should render as User message"
        );

        // The message should contain the original request and tool activity
        if let ChatMessage::User { content } = compaction_msg.unwrap() {
            let ContentBlock::Text { text } = &content[0];
            assert!(
                text.contains("fix the bug"),
                "should contain original request"
            );
            assert!(text.contains("read_file"), "should contain tool name");
            assert!(text.contains("1 calls"), "should contain call count");
            assert!(text.contains("src/main.rs"), "should contain args summary");
            assert!(
                text.contains("compacted for budget"),
                "should contain notes"
            );
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn path_messages_renders_branch_summary() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let from_id = session.append_user_message("hello");

        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(10),
            notes: None,
        };

        session
            .branch_with_summary(&root_id, summary, from_id)
            .unwrap();

        let messages = session.path_messages();

        // The branch summary should be rendered as a synthetic User message
        let summary_msg = messages.iter().find(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            summary_msg.is_some(),
            "branch summary should render as User message"
        );
    }

    #[test]
    fn path_messages_subtracts_tool_schema_overhead() {
        use crate::schema::ToolSchema;
        use serde_json::json;

        // Create a session with a very small budget and tool schemas
        let tools = vec![
            ToolSchema::function(
                "read_file",
                "Read a file",
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            ToolSchema::function(
                "run_command",
                "Execute a command",
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            ),
        ];

        let mut session = Session::new("m", Some("sys"), tools, "/tmp")
            .with_token_budget(TokenBudget::with_reserve(500, 50));

        // Add enough messages that some would need to be evicted
        for i in 0..20 {
            session.append_user_message(&format!(
                "message {i} with some padding text to make it longer"
            ));
            session.append_assistant_message(ChatMessage::assistant_text(format!("reply {i}")));
        }

        let messages = session.path_messages();

        // The messages should fit within the adjusted budget (which is smaller
        // than the raw budget because tool schema overhead was subtracted)
        // At minimum, the system message should survive
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System { .. })),
            "system message should always survive"
        );
    }

    #[test]
    fn path_messages_without_tools_has_no_schema_overhead() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let messages = session.path_messages();
        // With no tools and a generous budget, all messages should fit
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn path_messages_after_branch_excludes_old_branch() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        session.append_user_message("hello");
        let _asst_a_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to root
        session.branch_to(&root_id).unwrap();
        session.append_user_message("different question");

        let messages = session.path_messages();

        // Should contain: System, LeafMoved (Attached → filtered), User("different question")
        // Should NOT contain "reply A" or "hello"
        let has_reply_a = messages.iter().any(|m| {
            if let ChatMessage::Assistant {
                content,
                tool_calls,
            } = m
            {
                tool_calls.is_empty()
                    && content.iter().any(|b| {
                        let ContentBlock::Text { text } = b;
                        text.contains("reply A")
                    })
            } else {
                false
            }
        });
        assert!(!has_reply_a, "old branch should not appear");

        let has_different = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("different question")
                })
            } else {
                false
            }
        });
        assert!(has_different, "new question should appear");
    }

    #[test]
    fn path_messages_empty_session() {
        let session = Session::new("m", None, vec![], "/tmp");
        let messages = session.path_messages();
        assert!(messages.is_empty());
    }

    #[test]
    fn compaction_summary_rendering_is_deterministic() {
        use crate::context::render_compaction_summary;

        let mut tool_calls = std::collections::BTreeMap::new();
        tool_calls.insert(
            ToolName::from("read_file"),
            vec!["a.rs".to_owned(), "b.rs".to_owned()],
        );

        let summary = CompactionSummary {
            original_request: Some("fix it".to_owned()),
            tool_calls,
            tokens_compacted: 500,
            entry_count: 7,
            time_span: std::time::Duration::from_secs(30),
            notes: Some("notes here".to_owned()),
        };

        let msg1 = render_compaction_summary(&summary);
        let msg2 = render_compaction_summary(&summary);

        // BTreeMap guarantees iteration order, so rendering is deterministic
        assert_eq!(
            msg1, msg2,
            "rendering should be byte-stable for fixed input"
        );

        // Verify the text content contains the expected parts
        if let ChatMessage::User { content } = &msg1 {
            let ContentBlock::Text { text } = &content[0];
            assert!(text.contains("[Compacted: 7 entries, 500 tokens"));
            assert!(text.contains("Original request: \"fix it\""));
            assert!(text.contains("read_file: 2 calls"));
            assert!(text.contains("a.rs, b.rs"));
            assert!(text.contains("notes here"));
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn compaction_summary_rendering_without_optional_fields() {
        use crate::context::render_compaction_summary;

        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 2,
            time_span: std::time::Duration::from_secs(5),
            notes: None,
        };

        let msg = render_compaction_summary(&summary);
        if let ChatMessage::User { content } = &msg {
            let ContentBlock::Text { text } = &content[0];
            assert!(text.contains("[Compacted: 2 entries, 100 tokens"));
            assert!(!text.contains("Original request"));
            assert!(!text.contains("Tool activity"));
        } else {
            panic!("expected User message");
        }
    }

    // ── compact_older_than tests (Task 9) ───────────────────────────────

    #[tokio::test]
    async fn compact_older_than_transitions_resolution() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("fix the bug");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("ok"));
        let _user2_id = session.append_user_message("read another file");

        // Compact with a very low threshold (1 token) to force compaction
        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        // The compacted entries should now have Compacted resolution
        let user_entry = session.entry(&user_id).unwrap();
        assert!(
            matches!(
                &user_entry.resolution,
                EntryResolution::Compacted { into } if *into == compaction_id
            ),
            "compacted entry should have Compacted resolution pointing to the compaction entry"
        );

        // The compaction entry itself should be Full resolution
        let compaction_entry = session.entry(&compaction_id).unwrap();
        assert!(matches!(compaction_entry.resolution, EntryResolution::Full));
    }

    #[tokio::test]
    async fn compact_older_than_preserves_original_request() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("find the secret: TIGER-7742");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));
        session.append_user_message("now read another file");

        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        // The compaction summary should contain the original request
        let compaction_entry = session.entry(&compaction_id).unwrap();
        if let EntryPayload::Compaction { summary, .. } = &compaction_entry.payload {
            assert_eq!(
                summary.original_request,
                Some("find the secret: TIGER-7742".to_owned())
            );
        } else {
            panic!("expected Compaction payload");
        }
    }

    #[tokio::test]
    async fn compact_older_than_compacted_entries_filtered_from_path() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("fix the bug");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));
        session.append_user_message("read another file");

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        let messages = session.path_messages();

        // The compacted user message should NOT appear as a raw User message
        // in path_messages (it may appear inside the compaction summary text)
        let has_raw_user_msg = messages.iter().any(|m| {
            // We look for a User message that is NOT the compaction summary
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    // A raw user message would just be "fix the bug", not the
                    // formatted compaction summary
                    text.trim() == "fix the bug"
                })
            } else {
                false
            }
        });
        assert!(
            !has_raw_user_msg,
            "compacted entry should not appear as a raw User message in path_messages"
        );

        // The compaction summary SHOULD appear as a synthetic User message
        let has_compacted = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            has_compacted,
            "compaction summary should appear in path_messages"
        );

        // The non-compacted entries should still be present
        let has_another = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("read another file")
                })
            } else {
                false
            }
        });
        assert!(
            has_another,
            "non-compacted entry should appear in path_messages"
        );

        // Verify the entry is indeed Compacted
        let compacted_entry = session.entry(&user_id).unwrap();
        assert!(
            matches!(
                compacted_entry.resolution,
                EntryResolution::Compacted { .. }
            ),
            "compacted entry should have Compacted resolution"
        );
    }

    #[tokio::test]
    async fn compact_older_than_does_not_compact_root() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("system prompt"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The root (system message) should never be compacted
        let root_entry = session.entry(&root_id).unwrap();
        assert!(
            matches!(root_entry.resolution, EntryResolution::Full),
            "root (system message) should never be compacted"
        );
    }

    #[tokio::test]
    async fn compact_older_than_does_not_compact_leaf() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let leaf_before = session.leaf().unwrap();

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The leaf should have moved (to the compaction entry),
        // but the previous leaf entry itself should not be compacted
        // (it was the last entry before compaction was appended)
        let former_leaf = session.entry(&leaf_before).unwrap();
        // The former leaf was the assistant message, which should remain Full
        // since we don't compact the last entry
        assert!(
            matches!(former_leaf.resolution, EntryResolution::Full),
            "the last entry before compaction should not be compacted"
        );
    }

    #[tokio::test]
    async fn compact_older_than_errors_on_too_few_entries() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        // Only root entry — not enough to compact
        let strategy = MechanicalCompactionStrategy::new();
        let result = session.compact_older_than(1, &strategy).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compact_older_than_errors_when_threshold_not_exceeded() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hi");
        session.append_assistant_message(ChatMessage::assistant_text("hello"));

        // Set an enormous threshold that the entries won't reach
        let strategy = MechanicalCompactionStrategy::new();
        let result = session.compact_older_than(1_000_000, &strategy).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compact_older_than_compacted_entries_still_in_tree() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("fix the bug");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("ok"));
        session.append_user_message("read another file");

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The compacted entries should still be accessible via entry()
        assert!(
            session.entry(&user_id).is_some(),
            "compacted user entry should still exist"
        );
        assert!(
            session.entry(&asst_id).is_some(),
            "compacted assistant entry should still exist"
        );
    }

    #[tokio::test]
    async fn compact_older_than_first_kept_references_valid_entry() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("msg1");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply1"));
        let last_user_id = session.append_user_message("msg2");

        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        // The Compaction entry's first_kept should reference a valid entry
        let compaction_entry = session.entry(&compaction_id).unwrap();
        if let EntryPayload::Compaction { first_kept, .. } = &compaction_entry.payload {
            assert!(
                session.entry(first_kept).is_some(),
                "first_kept should reference a valid entry"
            );
            // With threshold=1, only the first entry after root gets compacted
            // (it exceeds threshold immediately), so first_kept is the assistant entry
            assert_eq!(
                *first_kept, asst_id,
                "first_kept should be the first entry after the compacted range"
            );
        } else {
            panic!("expected Compaction payload");
        }

        // The last user message should NOT be compacted
        let last_user = session.entry(&last_user_id).unwrap();
        assert!(
            matches!(last_user.resolution, EntryResolution::Full),
            "last entry before compaction should not be compacted"
        );
    }

    // ── ExtensionEntry tests (Task 10) ────────────────────────────────────

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct DiagnosticsState {
        error_count: usize,
        last_error: Option<String>,
    }

    impl ExtensionEntry for DiagnosticsState {
        const KIND: &'static str = "rho.diagnostics.v1";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct DiagnosticsStateV2 {
        error_count: usize,
        last_error: Option<String>,
        severity: String,
    }

    impl ExtensionEntry for DiagnosticsStateV2 {
        const KIND: &'static str = "rho.diagnostics.v2";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct LintSummary {
        warnings: usize,
        files_checked: usize,
    }

    impl ExtensionMessageEntry for LintSummary {
        const KIND: &'static str = "rho.lint-summary.v1";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct LintSummaryV2 {
        warnings: usize,
        files_checked: usize,
        errors: usize,
    }

    impl ExtensionMessageEntry for LintSummaryV2 {
        const KIND: &'static str = "rho.lint-summary.v2";
    }

    #[test]
    fn write_and_read_custom_state_round_trips() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        let state = DiagnosticsState {
            error_count: 3,
            last_error: Some("mismatched types".to_owned()),
        };
        let id = session.write_custom_state(&state);

        // The entry should have Custom payload with the correct kind
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Attached));
        if let EntryPayload::Custom { kind, data } = &entry.payload {
            assert_eq!(kind, "rho.diagnostics.v1");
            assert_eq!(data["error_count"], 3);
            assert_eq!(data["last_error"], "mismatched types");
        } else {
            panic!("expected Custom payload");
        }

        // Read back with the correct type
        let read_back: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(read_back, Some(state));
    }

    #[test]
    fn read_custom_state_returns_none_on_kind_mismatch() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        // Write as v1
        let state = DiagnosticsState {
            error_count: 1,
            last_error: None,
        };
        let id = session.write_custom_state(&state);

        // Try to read as v2 — different KIND, should get None
        let result: Option<DiagnosticsStateV2> = session.read_custom_state(&id);
        assert_eq!(result, None, "kind mismatch should return None");
    }

    #[test]
    fn read_custom_state_returns_none_on_nonexistent_entry() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result: Option<DiagnosticsState> = session.read_custom_state(&fake_id);
        assert_eq!(result, None, "nonexistent entry should return None");
    }

    #[test]
    fn read_custom_state_returns_none_on_wrong_payload_type() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        // Write a CustomMessage (Full resolution), not a Custom (Attached)
        let id = session.append_custom_message(
            "rho.diagnostics.v1".to_owned(),
            vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
        );

        // Try to read as Custom state — wrong payload type, should get None
        let result: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(result, None, "wrong payload type should return None");
    }

    #[test]
    fn write_custom_state_multiple_entries_independent() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        let state1 = DiagnosticsState {
            error_count: 1,
            last_error: Some("e1".to_owned()),
        };
        let state2 = DiagnosticsState {
            error_count: 2,
            last_error: Some("e2".to_owned()),
        };

        let id1 = session.write_custom_state(&state1);
        let id2 = session.write_custom_state(&state2);

        let read1: Option<DiagnosticsState> = session.read_custom_state(&id1);
        let read2: Option<DiagnosticsState> = session.read_custom_state(&id2);

        assert_eq!(read1, Some(state1));
        assert_eq!(read2, Some(state2));
    }

    #[test]
    fn write_and_read_custom_message_round_trips() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let id = session.write_custom_message(&summary);

        // The entry should have CustomMessage payload with the correct kind
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        if let EntryPayload::CustomMessage { kind, content } = &entry.payload {
            assert_eq!(kind, "rho.lint-summary.v1");
            assert!(!content.is_empty());
        } else {
            panic!("expected CustomMessage payload");
        }

        // Read back with the correct type
        let read_back: Option<LintSummary> = session.read_custom_message(&id);
        assert_eq!(read_back, Some(summary));
    }

    #[test]
    fn read_custom_message_returns_none_on_kind_mismatch() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        // Write as v1
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let id = session.write_custom_message(&summary);

        // Try to read as v2 — different KIND, should get None
        let result: Option<LintSummaryV2> = session.read_custom_message(&id);
        assert_eq!(result, None, "kind mismatch should return None");
    }

    #[test]
    fn read_custom_message_returns_none_on_nonexistent_entry() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result: Option<LintSummary> = session.read_custom_message(&fake_id);
        assert_eq!(result, None, "nonexistent entry should return None");
    }

    #[test]
    fn read_custom_message_returns_none_on_wrong_payload_type() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        // Write a Custom (Attached), not a CustomMessage (Full)
        let id = session.append_custom_state(
            "rho.lint-summary.v1".to_owned(),
            serde_json::json!({"warnings": 5}),
        );

        // Try to read as CustomMessage — wrong payload type, should get None
        let result: Option<LintSummary> = session.read_custom_message(&id);
        assert_eq!(result, None, "wrong payload type should return None");
    }

    #[test]
    fn extension_entry_kind_versioning_produces_clean_break() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");

        // Write v1
        let v1 = DiagnosticsState {
            error_count: 3,
            last_error: Some("type error".to_owned()),
        };
        let id = session.write_custom_state(&v1);

        // v1 reads back fine
        let v1_read: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(v1_read, Some(v1.clone()));

        // v2 reads None (clean break)
        let v2_read: Option<DiagnosticsStateV2> = session.read_custom_state(&id);
        assert_eq!(v2_read, None);

        // Now write v2 and verify v2 reads fine
        let v2 = DiagnosticsStateV2 {
            error_count: 3,
            last_error: Some("type error".to_owned()),
            severity: "high".to_owned(),
        };
        let id2 = session.write_custom_state(&v2);
        let v2_read2: Option<DiagnosticsStateV2> = session.read_custom_state(&id2);
        assert_eq!(v2_read2, Some(v2));

        // v1 cannot read the v2 entry
        let v1_read2: Option<DiagnosticsState> = session.read_custom_state(&id2);
        assert_eq!(v1_read2, None);
    }

    #[test]
    fn custom_state_is_filtered_from_path_messages() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Write a custom state entry (Attached resolution)
        let state = DiagnosticsState {
            error_count: 3,
            last_error: None,
        };
        session.write_custom_state(&state);

        let messages = session.path_messages();

        // Only System + User should appear; the Custom state is Attached
        assert_eq!(messages.len(), 2, "only System and User should appear");
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
    }

    #[test]
    fn custom_message_appears_in_path_messages() {
        let mut session = Session::new("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Write a custom message entry (Full resolution)
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        session.write_custom_message(&summary);

        let messages = session.path_messages();

        // System + User + CustomMessage (rendered as User)
        assert!(messages.len() >= 3, "custom message should appear in path");

        // The custom message should be rendered as a User message (by fit_path)
        let has_custom = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("warnings")
                })
            } else {
                false
            }
        });
        assert!(
            has_custom,
            "custom message content should be visible to the model"
        );
    }

    #[test]
    fn extension_message_entry_default_content_blocks_round_trips() {
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let blocks = summary.content_blocks();
        assert!(!blocks.is_empty());

        // Default implementation serializes to JSON
        let ContentBlock::Text { text } = &blocks[0];
        assert!(text.contains("warnings"), "should contain field names");

        // from_content_blocks should reconstruct the value
        let reconstructed: Option<LintSummary> = LintSummary::from_content_blocks(&blocks);
        assert_eq!(reconstructed, Some(summary));
    }

    #[test]
    fn extension_message_entry_from_empty_blocks_returns_none() {
        let result: Option<LintSummary> = LintSummary::from_content_blocks(&[]);
        assert_eq!(result, None, "empty blocks should return None");
    }
}
