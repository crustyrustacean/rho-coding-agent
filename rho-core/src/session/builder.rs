// Session construction, opening, builder methods, and mutators.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::context::{ContextManager, SlidingWindowContextManager, TokenBudget};
use crate::error::Result;
use crate::message::ChatMessage;
use crate::newtypes::{EntryId, SessionId};
use crate::redact::Redactor;

use super::Session;
use super::entry::{Entry, EntryPayload, EntryResolution};
use super::estimator::{HeuristicEstimator, TokenEstimator};
use super::header::SessionHeader;
use super::persist;
use super::persist::PersistState;

impl Session {
    /// Create a new session with JSONL persistence enabled.
    ///
    /// The system prompt (if provided) becomes the first [`Entry`] with
    /// `parent_id = None` and `resolution: Full`. The leaf pointer is set to
    /// this root entry.
    ///
    /// The session will auto-flush to `~/.rho/sessions/<project-hash>/` on
    /// each append operation. Use [`Session::in_memory`] to skip persistence.
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
                resolution: EntryResolution::Full,
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
            version: 1,
            created_at: SystemTime::now(),
            cwd: cwd.into(),
            parent_session: None,
        };

        let save_path = persist::compute_save_path(&header);

        Self {
            header,
            entries,
            append_order,
            leaf,
            estimator: Box::new(HeuristicEstimator::new()),
            model: model.into(),
            tools,
            context_manager: Box::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            details_store: HashMap::new(),
            persist: PersistState::with_path(save_path, 0),
        }
    }

    /// Create a new in-memory session that performs no disk I/O.
    ///
    /// This is the same as [`Session::new`] except no JSONL file is created
    /// and [`flush`](Session::flush) is a no-op. Used by tests and ephemeral
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
                resolution: EntryResolution::Full,
                payload: EntryPayload::Message(ChatMessage::system_text(prompt)),
            };
            let id = root.id.clone();
            entries.insert(id.clone(), root);
            append_order.push(id.clone());
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
            append_order,
            leaf,
            estimator: Box::new(HeuristicEstimator::new()),
            model: model.into(),
            tools,
            context_manager: Box::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            details_store: HashMap::new(),
            persist: PersistState::in_memory(),
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
    /// Returns [`RhoError`] if the file cannot be read, is malformed, or
    /// contains no entries.
    pub fn open(path: &Path) -> Result<Self> {
        persist::open_session(path)
    }

    /// Construct a `Session` from pre-built components.
    ///
    /// Used by [`open_session`](persist::open_session) to reconstruct a
    /// session from a JSONL file. This bypasses the normal constructor
    /// because the header, entries, and leaf are already known.
    pub(crate) fn new_internal(
        header: SessionHeader,
        entries: HashMap<EntryId, Entry>,
        leaf: Option<EntryId>,
        persist_state: PersistState,
    ) -> Self {
        // Reconstruct append_order from the entries: sort by timestamp
        // as a stable approximation of append order.
        let mut append_order: Vec<(std::time::SystemTime, EntryId)> = entries
            .values()
            .map(|e| (e.timestamp, e.id.clone()))
            .collect();
        append_order.sort_by_key(|a| a.0);
        let append_order: Vec<EntryId> = append_order.into_iter().map(|(_, id)| id).collect();

        Self {
            header,
            entries,
            append_order,
            leaf,
            estimator: Box::new(HeuristicEstimator::new()),
            model: String::new(), // Model is not persisted yet (Phase 2.6)
            tools: vec![],        // Tools are not persisted yet (Phase 2.6)
            context_manager: Box::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
            details_store: HashMap::new(),
            persist: persist_state,
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
    }

    /// Override the secret redactor (useful when resuming a session with
    /// different redaction settings).
    pub fn set_redactor(&mut self, redactor: Redactor) {
        self.redactor = redactor;
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
        let session = Session::in_memory("test-model", Some("you are helpful"), vec![], "/tmp");
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
        let session = Session::in_memory("test-model", None, vec![], "/tmp");
        assert!(session.leaf().is_none());
        assert_eq!(session.entry_count(), 0);
    }

    #[test]
    fn builder_methods_override_defaults() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(4096));
        assert_eq!(session.token_budget().context_window, 4096);
    }

    #[test]
    fn header_has_correct_version() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        assert_eq!(session.header().version, 1);
    }

    #[test]
    fn header_cwd_matches_constructor() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/project");
        assert_eq!(session.header().cwd, PathBuf::from("/project"));
    }

    #[test]
    fn header_session_id_is_unique() {
        let s1 = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let s2 = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        assert_ne!(s1.header().id, s2.header().id);
    }

    #[test]
    fn set_model_updates_model() {
        let mut session = Session::in_memory("old-model", Some("sys"), vec![], "/tmp");
        session.set_model("new-model");
        assert_eq!(session.model(), "new-model");
    }

    #[test]
    fn estimator_default_is_heuristic() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let tokens = session.estimator().estimate("hello");
        assert!(tokens > 0);
    }

    #[test]
    fn estimator_mut_allows_calibration() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.estimator_mut().calibrate("test-model", 10, 12);
    }

    #[test]
    fn open_nonexistent_file_returns_error() {
        let result = Session::open(Path::new("/nonexistent/path/session.jsonl"));
        assert!(result.is_err(), "opening nonexistent file should fail");
    }

    #[test]
    fn open_empty_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let result = Session::open(&path);
        assert!(result.is_err(), "opening empty file should fail");
    }

    #[test]
    fn open_file_with_only_header_returns_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header_only.jsonl");

        // Write just a header line
        let header = JsonlLine::Header {
            id: "abc12345".to_owned(),
            version: 1,
            created_at_secs: 1_700_000_000,
            cwd: "/tmp".to_owned(),
            parent_session: None,
        };
        let json = serde_json::to_string(&header).unwrap();
        std::fs::write(&path, format!("{json}\n")).unwrap();

        let result = Session::open(&path);
        // A header-only file has no entries — the session should be created
        // but with an empty tree.
        assert!(result.is_ok(), "header-only file should open");
        let session = result.unwrap();
        assert_eq!(session.entry_count(), 0);
        assert!(session.leaf().is_none());
    }
}
