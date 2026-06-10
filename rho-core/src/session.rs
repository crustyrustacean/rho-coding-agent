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
//! | [`compaction`] | [`CompactionStrategy`] trait, [`MechanicalCompactionStrategy`] |
//! | [`persist`] | [`PersistState`], JSONL session persistence |
//!
//! # Persistence
//!
//! Sessions persist to append-only JSONL files. Each line is a JSON object
//! (a header line followed by entry lines). The path layout is:
//! `~/.rho/sessions/<project-hash>/<timestamp>_<session-id>.jsonl`.
//!
//! - `Session::new` creates a persisted session that auto-flushes on every
//!   append operation.
//! - `Session::in_memory` creates a session with no disk I/O (for tests).
//! - `Session::open` reloads a session from a JSONL file.
//! - `Session::flush` writes any unwritten entries to disk.

pub mod accessors;
pub mod append;
pub mod builder;
pub mod compaction;
pub mod context;
pub mod context_stats;
pub mod entry;
pub mod error;
pub mod estimator;
pub mod eviction;
pub mod extensions;
pub mod header;
pub mod outliner;
pub mod persist;
pub mod phase;
pub mod tree;
pub mod truncation;

pub use compaction::{CompactionStrategy, LlmCompactionStrategy, MechanicalCompactionStrategy};
pub use context_stats::ContextStats;
pub use entry::{CompactionPhase, CompactionSummary, Entry, EntryPayload, EntryResolution};
pub use estimator::{HeuristicEstimator, TokenEstimator};
pub use extensions::{ExtensionEntry, ExtensionMessageEntry};
pub use header::SessionHeader;
pub use persist::PersistState;
pub use persist::{SessionMetadata, find_latest_session, list_sessions};
pub use persist::{default_save_path, open_session, project_hash};

use crate::context::{ContextManager, TokenBudget};
use crate::newtypes::EntryId;
use crate::redact::Redactor;

use crate::tool::ToolResultDetails;
use std::collections::HashMap;

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
/// `Session::in_memory` creates a session that skips all disk I/O, used by
/// tests and ephemeral sessions.
///
/// # Builder methods
///
/// Builder methods (`with_context_manager`, `with_token_budget`,
/// `with_redactor`, `with_estimator`) follow a fluent pattern for
/// overriding defaults after construction.
///
/// # Persistence
///
/// Sessions persist to JSONL via [`flush`](Session::flush). Each append
/// operation auto-flushes to disk, so a crashed process loses at most one
/// in-flight entry. The in-memory representation is the authoritative state;
/// the on-disk log is an append-only record of every entry.
///
/// Use [`open`](Session::open) to reload a previously persisted session.
pub struct Session {
    /// Session identity and origin metadata.
    header: SessionHeader,
    /// All entries in the session tree, indexed by ID.
    entries: HashMap<EntryId, Entry>,
    /// Append-ordered entry IDs (in the order they were added to the session).
    append_order: Vec<EntryId>,
    /// The current leaf position. Always `Some` after construction.
    leaf: Option<EntryId>,
    /// Token estimator for budget-aware decisions.
    estimator: Box<dyn TokenEstimator>,
    /// Model identifier.
    pub model: String,
    /// Reasoning effort for thinking-capable models.
    pub reasoning_effort: Option<String>,
    /// Tool schemas sent with every request.
    pub tools: Vec<rho_ai::ToolDefinition>,
    /// Context window manager applied before each request.
    context_manager: Box<dyn ContextManager>,
    /// Token budget for the context manager.
    token_budget: TokenBudget,
    /// Secret redactor applied to tool results before they enter history.
    redactor: Redactor,
    /// Full content of truncated tool results, indexed by entry ID.
    ///
    /// When a tool result exceeds the budget fraction and is truncated, the
    /// full (redacted) content is stored here so it can be retrieved later
    /// via [`get_full_result`](Session::get_full_result).
    details_store: HashMap<EntryId, ToolResultDetails>,
    /// Persistence state (save path, flushed count).
    persist: PersistState,
    /// Cached token overhead of the tool schemas.
    ///
    /// Set to `Some(n)` after the first computation and invalidated when
    /// [`set_tools`](Session::set_tools) changes the schema set. Uses
    /// [`Cell`] for interior mutability so the accessor remains `&self`.
    schema_overhead_cache: std::cell::Cell<Option<usize>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("header", &self.header)
            .field("entry_count", &self.entries.len())
            .field("leaf", &self.leaf)
            .field("model", &self.model)
            .field("token_budget", &self.token_budget)
            .field("persist", &self.persist)
            .finish_non_exhaustive()
    }
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
    use crate::message::{ChatMessage, ContentBlock};
    use crate::session::context_stats::{
        PhaseTokenDistribution, ResolutionTokenDistribution, RoleTokenDistribution,
    };

    // ── TokenBudget tests ──────────────────────────────────────────────

    #[test]
    fn token_budget_prompt_budget_subtracts_reserve() {
        let budget = TokenBudget::with_reserve(32_768, 4096);
        assert_eq!(budget.prompt_budget(), 28_672);
    }

    #[test]
    fn token_budget_default_has_32k_window_and_8k_reserve() {
        let budget = TokenBudget::default();
        assert_eq!(budget.context_window, 32_768);
        assert_eq!(budget.completion_reserve, 8192);
        assert_eq!(budget.prompt_budget(), 24_576);
    }

    #[test]
    fn token_budget_max_tokens_is_context_window() {
        let budget = TokenBudget::new(16_384);
        assert_eq!(budget.max_tokens(), 16_384);
        assert_eq!(budget.context_window, 16_384);
    }
    // ── JSONL Persistence tests (Task 11) ────────────────────────────────────

    use crate::session::persist::{JsonlLine, default_save_path, project_hash};
    use std::path::{Path, PathBuf};

    #[test]
    fn in_memory_session_has_no_save_path() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        assert!(
            session.save_path().is_none(),
            "in-memory session should have no save path"
        );
    }

    #[test]
    fn persisted_session_has_save_path() {
        let session = Session::new("m", Some("sys"), vec![], "/tmp");
        assert!(
            session.save_path().is_some(),
            "persisted session should have a save path"
        );
        let path = session.save_path().unwrap();
        assert!(
            path.to_string_lossy().contains("sessions"),
            "save path should contain 'sessions' directory"
        );
        assert!(
            path.extension().is_some_and(|e| e == "jsonl"),
            "save path should have .jsonl extension"
        );
    }

    #[test]
    fn in_memory_flush_is_noop() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        // Should succeed even though there's no file
        assert!(session.flush().is_ok());
    }

    #[test]
    fn project_hash_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let hash1 = project_hash(cwd);
        let hash2 = project_hash(cwd);
        assert_eq!(hash1, hash2, "project hash should be stable");
        assert_eq!(hash1.len(), 16, "project hash should be 16 chars");
    }

    #[test]
    fn project_hash_differs_for_different_cwds() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let hash1 = project_hash(dir1.path());
        let hash2 = project_hash(dir2.path());
        assert_ne!(
            hash1, hash2,
            "different CWDs should produce different hashes"
        );
    }

    #[test]
    fn project_hash_is_hex() {
        let dir = tempfile::tempdir().unwrap();
        let hash = project_hash(dir.path());
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "project hash should be hex: {hash}"
        );
    }

    #[test]
    fn default_save_path_layout() {
        let path = default_save_path(Path::new("/my/project"), "abc12345", 1_700_000_000);
        let path_str = path.to_string_lossy();
        // Should contain ~/.rho/sessions/<hash>/<timestamp>_<session-id>.jsonl
        assert!(path_str.contains("sessions"));
        assert!(path_str.contains("1700000000_abc12345.jsonl"));
    }

    #[test]
    fn jsonl_line_header_round_trips() {
        let header = JsonlLine::Header {
            id: "abc12345".to_owned(),
            version: 1,
            created_at_secs: 1_700_000_000,
            cwd: "/my/project".to_owned(),
            parent_session: None,
        };
        let json = serde_json::to_string(&header).unwrap();
        let back: JsonlLine = serde_json::from_str(&json).unwrap();
        assert_eq!(header, back);
    }

    #[test]
    fn jsonl_line_entry_round_trips() {
        let entry = Entry {
            id: EntryId::from("test1234"),
            parent_id: None,
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::system_text("hello")),
        };
        let line = JsonlLine::Entry(entry.clone());
        let json = serde_json::to_string(&line).unwrap();
        let back: JsonlLine = serde_json::from_str(&json).unwrap();
        assert_eq!(line, back);
    }

    #[test]
    fn save_reopen_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_session.jsonl");

        // Create a session, add entries, and flush.
        let mut session = Session::in_memory("test-model", Some("you are helpful"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("find the secret: TIGER-7742");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("ok"));
        let _user2_id = session.append_user_message("read another file");

        // Manually set the save path and flushed count to simulate persistence.
        // We can't use Session::new directly in tests because it writes to ~/.rho,
        // so we use in_memory + manual flush.
        session.persist.save_path = Some(path.clone());
        session.persist.flushed_count = 0;

        // Flush to disk
        session.flush().unwrap();

        // Verify the file exists
        assert!(path.exists(), "session file should exist after flush");

        // Read back
        let reopened = Session::open(&path).unwrap();

        // Verify header
        assert_eq!(reopened.header().id, session.header().id);
        assert_eq!(reopened.header().version, 1);
        assert_eq!(reopened.header().cwd, PathBuf::from("/tmp"));

        // Verify entries
        assert_eq!(reopened.entry_count(), session.entry_count());

        // Verify the leaf
        assert_eq!(reopened.leaf(), session.leaf());

        // Verify specific entries are accessible
        assert!(reopened.entry(&root_id).is_some());
        assert!(reopened.entry(&user_id).is_some());

        // Verify entry content
        let root_entry = reopened.entry(&root_id).unwrap();
        assert!(matches!(root_entry.resolution, EntryResolution::Full));

        let user_entry = reopened.entry(&user_id).unwrap();
        if let EntryPayload::Message(ChatMessage::User { content }) = &user_entry.payload {
            let ContentBlock::Text { text } = &content[0];
            assert!(
                text.contains("TIGER-7742"),
                "user message content should survive round-trip"
            );
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn flush_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("session.jsonl");

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.persist.save_path = Some(nested.clone());
        session.persist.flushed_count = 0;

        session.flush().unwrap();

        assert!(nested.exists(), "flush should create parent directories");
    }

    #[test]
    fn incremental_flush_appends_new_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incremental.jsonl");

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.persist.save_path = Some(path.clone());
        session.persist.flushed_count = 0;

        // First flush writes the header + root entry
        session.flush().unwrap();
        let file_size_1 = std::fs::metadata(&path).unwrap().len();
        assert!(file_size_1 > 0);

        // Append a user message (in-memory, since we're not using auto-flush)
        session.append_user_message("hello");
        // Manually flush the new entry
        session.flush().unwrap();
        let file_size_2 = std::fs::metadata(&path).unwrap().len();
        assert!(
            file_size_2 > file_size_1,
            "file should grow after appending entries"
        );

        // Read back and verify
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.entry_count(), 2, "should have system + user entry");
    }

    #[test]
    fn resolution_levels_survive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resolution.jsonl");

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("find the secret: TIGER-7742");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("ok"));

        // Manually compact the user entry (simulate compaction)
        if let Some(entry) = session.entries.get_mut(&user_id) {
            entry.resolution = EntryResolution::Compacted {
                into: EntryId::from("compaction"),
            };
        }

        session.persist.save_path = Some(path.clone());
        session.persist.flushed_count = 0;
        session.flush().unwrap();

        let reopened = Session::open(&path).unwrap();
        let user_entry = reopened.entry(&user_id).unwrap();
        assert!(
            matches!(&user_entry.resolution, EntryResolution::Compacted { into } if *into == EntryId::from("compaction")),
            "compacted resolution should survive round-trip"
        );
    }

    #[test]
    fn branch_operation_survives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("branch.jsonl");

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let _user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to root
        session.branch_to(&root_id).unwrap();

        session.persist.save_path = Some(path.clone());
        session.persist.flushed_count = 0;
        session.flush().unwrap();

        let reopened = Session::open(&path).unwrap();

        // The leaf should be at the LeafMoved entry
        let leaf_id = reopened.leaf().unwrap();
        let leaf_entry = reopened.entry(&leaf_id).unwrap();
        assert!(
            matches!(leaf_entry.payload, EntryPayload::LeafMoved { .. }),
            "leaf should be a LeafMoved entry after branch"
        );
    }

    #[test]
    fn all_payload_types_round_trip_via_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("all_types.jsonl");

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply"));
        session.append_label(root_id.clone(), Some("checkpoint".to_owned()));
        session.append_custom_state("rho.test.v1".to_owned(), serde_json::json!({"count": 42}));
        session.append_custom_message(
            "rho.test-msg.v1".to_owned(),
            vec![ContentBlock::Text {
                text: "test message".to_owned(),
            }],
        );

        // Append a compaction entry
        let summary = CompactionSummary {
            original_request: Some("hello".to_owned()),
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 2,
            time_span: std::time::Duration::from_secs(10),
            notes: None,
            key_findings: std::collections::BTreeMap::new(),
            phases: Vec::new(),
        };
        session.append_compaction(summary, asst_id.clone(), 200);

        // Branch back and add a branch summary
        session.branch_to(&user_id).unwrap();
        let branch_summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(5),
            notes: None,
            key_findings: std::collections::BTreeMap::new(),
            phases: Vec::new(),
        };
        session.append_branch_summary(branch_summary, EntryId::from("old_leaf"));

        session.close("test completed");

        session.persist.save_path = Some(path.clone());
        session.persist.flushed_count = 0;
        session.flush().unwrap();

        let reopened = Session::open(&path).unwrap();

        // Verify all entries survived
        assert_eq!(reopened.entry_count(), session.entry_count());

        // Check specific entry types
        let root_entry = reopened.entry(&root_id).unwrap();
        assert!(matches!(
            root_entry.payload,
            EntryPayload::Message(ChatMessage::System { .. })
        ));

        let user_entry = reopened.entry(&user_id).unwrap();
        assert!(matches!(
            user_entry.payload,
            EntryPayload::Message(ChatMessage::User { .. })
        ));

        let asst_entry = reopened.entry(&asst_id).unwrap();
        assert!(matches!(
            asst_entry.payload,
            EntryPayload::Message(ChatMessage::Assistant { .. })
        ));

        // Verify SessionEnded entry survived
        let leaf = reopened.leaf().unwrap();
        let leaf_entry = reopened.entry(&leaf).unwrap();
        assert_eq!(leaf_entry.resolution, EntryResolution::Attached);
        assert!(matches!(
            &leaf_entry.payload,
            EntryPayload::SessionEnded { reason } if reason == "test completed"
        ));
    }

    #[test]
    fn in_memory_session_flush_does_nothing() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // flush should be a no-op for in-memory sessions
        assert!(session.flush().is_ok());
        assert!(session.save_path().is_none());
    }

    #[test]
    fn jsonl_line_discriminant_tags() {
        // Verify the `type` tag is present and correct in serialized form
        let header = JsonlLine::Header {
            id: "abc".to_owned(),
            version: 1,
            created_at_secs: 0,
            cwd: "/tmp".to_owned(),
            parent_session: None,
        };
        let json = serde_json::to_string(&header).unwrap();
        assert!(
            json.contains("\"type\":\"Header\""),
            "Header should have type tag: {json}"
        );

        let entry = Entry {
            id: EntryId::from("test1234"),
            parent_id: None,
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Message(ChatMessage::system_text("hello")),
        };
        let entry_line = JsonlLine::Entry(entry);
        let json = serde_json::to_string(&entry_line).unwrap();
        assert!(
            json.contains("\"type\":\"Entry\""),
            "Entry should have type tag: {json}"
        );
    }

    #[test]
    fn auto_flush_writes_on_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto_flush.jsonl");

        // Use Session::new which has auto-flush enabled.
        // We need a temp dir for the CWD so the session path is writable.
        let cwd = dir.path();

        let mut session = Session::new("m", Some("sys"), vec![], cwd);
        // Redirect save path to our temp location
        session.persist.save_path = Some(path.clone());
        // Reset flushed_count since we changed the path after initial auto-flush
        // (the initial root entry was auto-flushed to the original path)
        session.persist.flushed_count = 0;

        session.append_user_message("hello");

        // Auto-flush should have written to disk
        assert!(path.exists(), "auto-flush should create the file");

        // Read back and verify
        let reopened = Session::open(&path).unwrap();
        assert!(
            reopened.entry_count() >= 2,
            "should have system + user entries, got {}",
            reopened.entry_count()
        );
    }

    #[test]
    fn context_stats_utilization_calculation() {
        let stats = crate::session::ContextStats {
            context_window: 32_768,
            completion_reserve: 8192,
            estimated_used: 12_288,
            message_count: 10,
            entry_count: 15,
            path_entry_count: 12,
            role_tokens: RoleTokenDistribution::default(),
            resolution_tokens: ResolutionTokenDistribution::default(),
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        // prompt_budget = 32_768 - 8192 = 24_576
        // utilization = 12_288 / 24_576 = 50%
        assert_eq!(stats.utilization_percent(), 50);
        // remaining = 24_576 - 12_288 = 12_288
        assert_eq!(stats.estimated_remaining(), 12_288);
    }

    #[test]
    fn context_stats_zero_budget() {
        let stats = crate::session::ContextStats {
            context_window: 0,
            completion_reserve: 0,
            estimated_used: 0,
            message_count: 0,
            entry_count: 0,
            path_entry_count: 0,
            role_tokens: RoleTokenDistribution::default(),
            resolution_tokens: ResolutionTokenDistribution::default(),
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        assert_eq!(stats.utilization_percent(), 100); // zero budget = full
        assert_eq!(stats.estimated_remaining(), 0);
    }

    #[test]
    fn context_stats_over_budget() {
        let stats = crate::session::ContextStats {
            context_window: 1000,
            completion_reserve: 200,
            estimated_used: 1500, // over budget
            message_count: 5,
            entry_count: 5,
            path_entry_count: 5,
            role_tokens: RoleTokenDistribution::default(),
            resolution_tokens: ResolutionTokenDistribution::default(),
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        assert_eq!(stats.utilization_percent(), 100); // capped at 100
        assert_eq!(stats.estimated_remaining(), 0); // saturates at 0
    }
}
