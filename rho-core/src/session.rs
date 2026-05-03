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
use crate::newtypes::{EntryId, SessionId};
use crate::redact::Redactor;
use crate::schema::ToolSchema;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

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
    #[expect(dead_code, reason = "used by send_current in Task 8")]
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
            estimator: Box::new(HeuristicEstimator),
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(session.token_budget().max_tokens, 4096);
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
        // Should not panic — validates &mut dyn TokenEstimator works.
        session.estimator_mut().calibrate("test-model", 10, 12);
    }
}
