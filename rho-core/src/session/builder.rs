// Session construction, opening, builder methods, and mutators.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::context::{ContextManager, SlidingWindowContextManager, TokenBudget};
use crate::error::Result;
use crate::message::ChatMessage;
use crate::newtypes::{EntryId, SessionId};
use crate::redact::Redactor;

use super::entry::{Entry, EntryPayload, EntryResolution};
use super::estimator::{HeuristicEstimator, TokenEstimator};
use super::header::SessionHeader;
use super::persist;
use super::persist::PersistState;
use super::{Cursor, SessionLog};

impl Cursor {
    /// Create a new session with JSONL persistence enabled.
    ///
    /// The system prompt (if provided) becomes the first [`Entry`] with
    /// `parent_id = None` and `resolution: Full`. The leaf pointer is set to
    /// this root entry.
    ///
    /// The session will auto-flush to `~/.rho/sessions/<project-hash>/` on
    /// each append operation. Use [`Cursor::in_memory`] to skip persistence.
    pub fn new(
        model: impl Into<String>,
        system_prompt: Option<&str>,
        tools: Vec<rho_ai::ToolDefinition>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let mut entries = HashMap::new();
        let mut append_order = Vec::new();
        let leaf = if let Some(prompt) = system_prompt {
            let root = Entry {
                id: EntryId::new(),
                parent_id: None,
                timestamp: SystemTime::now(),
                payload: EntryPayload::Message(ChatMessage::system_text(prompt)),
            };
            let id = root.id.clone();
            entries.insert(id.clone(), root);
            append_order.push(id.clone());
            Some(id)
        } else {
            None
        };

        let header = SessionHeader {
            id: SessionId::new(),
            version: super::persist::SESSION_FORMAT_VERSION,
            created_at: SystemTime::now(),
            cwd: cwd.into(),
            parent_session: None,
        };

        let save_path = persist::compute_save_path(&header);

        // Build the log through `insert` so the children index is populated
        // alongside the entries, then attach persistence.
        let mut log = SessionLog::new(header);
        for id in append_order {
            if let Some(entry) = entries.get(&id) {
                log.insert(entry.clone());
            }
        }
        log.persist = PersistState::with_path(save_path, 0);

        Self {
            log: std::sync::Arc::new(std::sync::Mutex::new(log)),
            resolution: HashMap::new(),
            leaf,
            cursor_id: crate::newtypes::CursorId::new(),
            estimator: std::sync::Arc::new(HeuristicEstimator::new()),
            model: model.into(),
            reasoning_effort: None,
            tools,
            context_manager: std::sync::Arc::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            schema_overhead_cache: std::sync::Mutex::new(None),
            api_usage: crate::session::context_stats::ApiUsage::default(),
            user_models: Vec::new(),
        }
    }

    /// Create a new in-memory session that performs no disk I/O.
    ///
    /// This is the same as [`Cursor::new`] except no JSONL file is created
    /// and [`flush`](Cursor::flush) is a no-op. Used by tests and ephemeral
    /// sessions.
    pub fn in_memory(
        model: impl Into<String>,
        system_prompt: Option<&str>,
        tools: Vec<rho_ai::ToolDefinition>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let mut entries = HashMap::new();
        let mut append_order = Vec::new();
        let leaf = if let Some(prompt) = system_prompt {
            let root = Entry {
                id: EntryId::new(),
                parent_id: None,
                timestamp: SystemTime::now(),
                payload: EntryPayload::Message(ChatMessage::system_text(prompt)),
            };
            let id = root.id.clone();
            entries.insert(id.clone(), root);
            append_order.push(id.clone());
            Some(id)
        } else {
            None
        };

        let mut log = SessionLog::new(SessionHeader {
            id: SessionId::new(),
            version: persist::SESSION_FORMAT_VERSION,
            created_at: SystemTime::now(),
            cwd: cwd.into(),
            parent_session: None,
        });
        for id in append_order {
            if let Some(entry) = entries.get(&id) {
                log.insert(entry.clone());
            }
        }

        Self {
            log: std::sync::Arc::new(std::sync::Mutex::new(log)),
            resolution: HashMap::new(),
            leaf,
            cursor_id: crate::newtypes::CursorId::new(),
            estimator: std::sync::Arc::new(HeuristicEstimator::new()),
            model: model.into(),
            reasoning_effort: None,
            tools,
            context_manager: std::sync::Arc::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            schema_overhead_cache: std::sync::Mutex::new(None),
            api_usage: crate::session::context_stats::ApiUsage::default(),
            user_models: Vec::new(),
        }
    }

    /// Open a session from a JSONL file.
    ///
    /// Reads all entries from the file, reconstructs the entry tree, and
    /// determines the leaf position. See [`persist::open_session`] for
    /// the full reconstruction logic.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::RhoError`] if the file cannot be read, is malformed, or
    /// contains no entries.
    pub fn open(path: &Path) -> Result<Self> {
        persist::open_session(path)
    }

    /// Construct a `Session` from pre-built components, seeding the
    /// resolution overlay.
    ///
    /// Used by [`open_session`](persist::open_session) to reconstruct a
    /// session from JSONL, including any `Resolution` lines found in the file
    /// (and the embedded resolutions of a v1 file). Bypasses the normal
    /// constructor because the header, entries, leaf, and overlay are already
    /// known.
    ///
    /// `append_order` is supplied by the caller in **file order**. It must not
    /// be reconstructed by sorting `entries`: the JSONL log is append-only, so
    /// line order *is* the append order, and a stable sort by timestamp is
    /// non-deterministic whenever two entries share a timestamp (common on
    /// Windows, where clock granularity is coarse) because the input order
    /// comes from a `HashMap` iteration that is randomised per process. See
    /// issue #29.
    pub(crate) fn new_internal_with_overlay(
        header: SessionHeader,
        entries: &HashMap<EntryId, Entry>,
        append_order: Vec<EntryId>,
        leaf: Option<EntryId>,
        persist_state: PersistState,
        resolution: HashMap<EntryId, EntryResolution>,
        cursor_id: crate::newtypes::CursorId,
    ) -> Self {
        debug_assert_eq!(
            append_order.len(),
            entries.len(),
            "append_order must list every entry exactly once"
        );

        // Rebuild the log from the supplied file order, which re-derives the
        // children index from `parent_id` (the index is not persisted).
        let mut log = SessionLog::new(header);
        log.persist = persist_state;
        for id in append_order {
            if let Some(entry) = entries.get(&id) {
                log.insert(entry.clone());
            }
        }

        Self {
            log: std::sync::Arc::new(std::sync::Mutex::new(log)),
            resolution,
            leaf,
            cursor_id,
            estimator: std::sync::Arc::new(HeuristicEstimator::new()),
            model: String::new(), // Model is not persisted yet (Phase 2.6)
            reasoning_effort: None,
            tools: vec![], // Tools are not persisted yet (Phase 2.6)
            context_manager: std::sync::Arc::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            schema_overhead_cache: std::sync::Mutex::new(None),
            api_usage: crate::session::context_stats::ApiUsage::default(),
            user_models: Vec::new(),
        }
    }

    // ── Builder methods ───────────────────────────────────────────────────

    /// Override the context manager.
    #[must_use]
    pub fn with_context_manager(mut self, cm: std::sync::Arc<dyn ContextManager>) -> Self {
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

    /// Set the reasoning effort for thinking-capable models.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Set user-defined model pricing entries (consulted by `route_response`
    /// before the built-in catalog).
    #[must_use]
    pub fn with_user_models(mut self, user_models: Vec<rho_ai::Model>) -> Self {
        self.user_models = user_models;
        self
    }

    /// Override the token estimator.
    #[must_use]
    pub fn with_estimator(mut self, estimator: std::sync::Arc<dyn TokenEstimator>) -> Self {
        self.estimator = estimator;
        self
    }

    // ── Mutators ──────────────────────────────────────────────────────────

    /// Switch to a different model.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    /// Override the token budget (useful when resuming a session with
    /// different budget settings).
    pub fn set_token_budget(&mut self, budget: TokenBudget) {
        self.token_budget = budget;
    }

    /// Override the tool schemas (useful when resuming a session with
    /// a different set of tools).
    pub fn set_tools(&mut self, tools: Vec<rho_ai::ToolDefinition>) {
        self.tools = tools;
        *self
            .schema_overhead_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Override the secret redactor (useful when resuming a session with
    /// different redaction settings).
    pub fn set_redactor(&mut self, redactor: Redactor) {
        self.redactor = redactor;
    }

    /// Override the user-defined model pricing entries (useful when resuming
    /// or starting a fresh session with the same pricing overrides).
    pub fn set_user_models(&mut self, user_models: Vec<rho_ai::Model>) {
        self.user_models = user_models;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TokenBudget;
    use crate::session::entry::{EntryPayload, EntryResolution};
    use crate::session::persist::JsonlLine;
    use std::path::PathBuf;

    #[test]
    fn new_session_with_system_prompt_has_root_entry() {
        let session = Cursor::in_memory("test-model", Some("you are helpful"), vec![], "/tmp");
        let leaf = session
            .leaf()
            .expect("leaf should be set after construction");
        let entry = session.entry(&leaf).expect("root entry should exist");
        assert!(entry.parent_id.is_none(), "root entry has no parent");
        assert!(matches!(
            EntryResolution::default_for(&entry.payload),
            EntryResolution::Full
        ));
        assert!(matches!(
            entry.payload,
            EntryPayload::Message(ChatMessage::System { .. })
        ));
    }

    #[test]
    fn new_session_without_system_prompt_has_no_entries() {
        let session = Cursor::in_memory("test-model", None, vec![], "/tmp");
        assert!(session.leaf().is_none());
        assert_eq!(session.entry_count(), 0);
    }

    #[test]
    fn builder_methods_override_defaults() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(4096));
        assert_eq!(session.token_budget().context_window, 4096);
    }

    #[test]
    fn with_user_models_sets_entries() {
        let model = rho_ai::Model {
            id: "custom/id".to_string(),
            name: "custom/id".to_string(),
            provider: "custom".to_string(),
            context_window: 0,
            max_tokens: 0,
            input: rho_ai::ModelInput::default(),
            cost: rho_ai::ModelCost {
                input: 1.0,
                output: 2.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            thinking: rho_ai::ModelThinking::default(),
        };
        let session =
            Cursor::in_memory("m", Some("sys"), vec![], "/tmp").with_user_models(vec![model]);
        assert_eq!(session.user_models.len(), 1);
        assert_eq!(session.user_models[0].id, "custom/id");
    }

    #[test]
    fn header_has_correct_version() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        assert_eq!(session.header().version, persist::SESSION_FORMAT_VERSION);
    }

    #[test]
    fn header_cwd_matches_constructor() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/project");
        assert_eq!(session.header().cwd, PathBuf::from("/project"));
    }

    #[test]
    fn header_session_id_is_unique() {
        let s1 = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let s2 = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        assert_ne!(s1.header().id, s2.header().id);
    }

    #[test]
    fn set_model_updates_model() {
        let mut session = Cursor::in_memory("old-model", Some("sys"), vec![], "/tmp");
        session.set_model("new-model");
        assert_eq!(session.model(), "new-model");
    }

    #[test]
    fn estimator_default_is_heuristic() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let tokens = session.estimator().estimate("test-model", "hello");
        assert!(tokens > 0);
    }

    #[test]
    fn estimator_allows_calibration_through_shared_ref() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        // `calibrate` takes `&self` so it works through a shared reference
        // (the estimator is held as `Arc<dyn TokenEstimator>`).
        session.estimator().calibrate("test-model", 10, 12);
    }

    // ── Rename guards (A3 step 4) ─────────────────────────────────────

    /// Compile-time guard: `Cursor` is the real type and `Session` is a
    /// compatibility alias for it.
    ///
    /// #65 keeps `pub type Session = Cursor` for one release so downstream
    /// embedders are not broken. This test fails to compile if the alias is
    /// dropped early, and documents the deprecation window rather than
    /// leaving it to be noticed at a use site.
    #[test]
    fn session_is_an_alias_of_cursor() {
        fn accepts_cursor(_: &crate::session::Cursor) {}
        fn accepts_session_alias(_: &crate::session::Session) {}

        let cursor = crate::session::Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        accepts_cursor(&cursor);
        accepts_session_alias(&cursor);
    }

    /// The alias is a true alias, not a second type: a value built as
    /// `Session` is usable everywhere a `Cursor` is expected.
    #[test]
    fn session_alias_constructs_and_forks() {
        let session: crate::session::Session =
            crate::session::Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let cursor: crate::session::Cursor = session.clone();
        // `clone` copies the cursor id — a clone continues the same cursor.
        // `fork` is what mints a new one.
        assert_eq!(session.cursor_id(), cursor.cursor_id());
        assert_ne!(
            session.cursor_id(),
            session.fork().cursor_id(),
            "forking must mint a fresh cursor id"
        );
    }

    // ── Clone / shared-log semantics (A3 PR 2) ──────────────────────────

    /// Compile-time guard: `Cursor` must stay `Clone`.
    ///
    /// Cloning is what makes "two cursors over one log" expressible. If this
    /// stops compiling, the log is no longer shareable.
    #[test]
    fn cursor_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<Cursor>();
    }

    /// A clone shares the log (entries, index, persistence) but owns its own
    /// leaf and resolution overlay.
    #[test]
    fn clone_shares_log_but_not_leaf() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root = session.leaf().unwrap();
        // Append first: `branch_to` is a documented no-op when the target is
        // already the leaf, so branching from the root would leave both
        // cursors on the same node and assert nothing.
        session.append_user_message("first turn");
        let before = session.entry_count();

        let mut forked = session.clone();
        let original_leaf = session.leaf();
        forked.branch_to(&root).unwrap();

        // The log is shared: the fork's LeafMoved entry is visible from the
        // original session.
        assert_eq!(
            session.entry_count(),
            forked.entry_count(),
            "clones must share the same log"
        );
        assert_eq!(
            session.entry_count(),
            before + 1,
            "the branch writes one LeafMoved entry to the shared log"
        );
        assert_ne!(
            session.leaf(),
            forked.leaf(),
            "each cursor owns its own leaf"
        );
        // The original cursor is unaffected by the fork's leaf move.
        assert_eq!(
            session.leaf(),
            original_leaf,
            "the fork must not move the original cursor's leaf"
        );
    }

    /// A cursor's resolution overlay is its own: pinning on one cursor does
    /// not change what another cursor over the same log sees.
    #[test]
    fn clone_has_independent_resolution_overlay() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("pin me");
        let forked = session.clone();

        session.pin_entry(&user_id).unwrap();

        assert!(
            matches!(session.resolution_of(&user_id), EntryResolution::Pinned),
            "the pinning cursor sees Pinned"
        );
        assert!(
            matches!(forked.resolution_of(&user_id), EntryResolution::Full),
            "a sibling cursor is unaffected — the overlay is per-cursor"
        );
    }

    /// Entries appended through one clone are visible from the other.
    #[test]
    fn append_on_one_clone_visible_by_the_other() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let before = session.entry_count();
        let forked = session.clone();

        session.append_user_message("only on the original");
        assert_eq!(
            forked.entry_count(),
            before + 1,
            "an append through one clone must be visible to the other"
        );
    }

    /// The estimator is shared: calibration through one clone improves the
    /// ratios seen by the other. Calibration is a property of the model, not
    /// of a branch, so this sharing is intended.
    #[test]
    fn clone_shares_estimator_state() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let forked = session.clone();

        let content = "x".repeat(1000);
        let before = session.estimator().estimate("shared-model", &content);
        session
            .estimator()
            .calibrate("shared-model", before, before * 2);
        let after = forked.estimator().estimate("shared-model", &content);

        assert_ne!(
            before, after,
            "calibration through one clone must be visible via the other"
        );
    }

    #[test]
    fn open_nonexistent_file_returns_error() {
        let result = Cursor::open(Path::new("/nonexistent/path/session.jsonl"));
        assert!(result.is_err(), "opening nonexistent file should fail");
    }

    #[test]
    fn open_empty_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let result = Cursor::open(&path);
        assert!(result.is_err(), "opening empty file should fail");
    }

    #[test]
    fn open_file_with_only_header_returns_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header_only.jsonl");

        // Write just a header line
        let header = JsonlLine::Header {
            id: "abc12345".to_owned(),
            version: super::persist::SESSION_FORMAT_VERSION,
            created_at_secs: 1_700_000_000,
            cwd: "/tmp".to_owned(),
            parent_session: None,
        };
        let json = serde_json::to_string(&header).unwrap();
        std::fs::write(&path, format!("{json}\n")).unwrap();

        let result = Cursor::open(&path);
        // A header-only file has no entries — the session should be created
        // but with an empty tree.
        assert!(result.is_ok(), "header-only file should open");
        let session = result.unwrap();
        assert_eq!(session.entry_count(), 0);
        assert!(session.leaf().is_none());
    }
}
