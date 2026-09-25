//! The append-only session log.
//!
//! [`SessionLog`] holds everything a session shares across branches: the
//! header, the entry tree, append order, the parent→children index, the
//! truncated-output store, and persistence state. Entries are immutable once
//! inserted — mutable per-entry state (resolution) lives in a separate overlay
//! owned by the session, and per-branch state (leaf, budget, model) is planned
//! to move into a per-branch cursor.
//!
//! # Why a separate type
//!
//! A session is currently a single linear conversation: one log *and* all
//! runtime state bundled together, so a session can be owned by exactly one
//! agent. Splitting the shared, append-only part out makes that boundary
//! explicit and is the first step toward a shared log with per-branch cursors
//! (see issue #65).
//!
//! # Invariants
//!
//! - `entries` and `order` are kept in sync: every id in `entries` appears
//!   exactly once in `order`, and `order` is the append (JSONL file) order.
//! - `children` mirrors `parent_id` for every entry, so `children()` is an
//!   index lookup rather than a scan over all entries.
//!
//! [`SessionLog`] is `pub` so embedders can inspect or build one directly, but
//! it is constructed through [`Session`](super::Session)'s API.

use crate::newtypes::EntryId;
use crate::session::entry::Entry;
use crate::session::header::SessionHeader;
use crate::session::persist::PersistState;
use crate::tool::ToolResultDetails;
use std::collections::HashMap;

/// The append-only, branch-shared portion of a session.
///
/// See the [module documentation](self) for the invariants and rationale.
#[derive(Debug)]
pub struct SessionLog {
    /// Session identity and origin metadata.
    pub(crate) header: SessionHeader,
    /// All entries in the session tree, indexed by ID.
    pub(crate) entries: HashMap<EntryId, Entry>,
    /// Append-ordered entry IDs, in the order they were added to the session.
    ///
    /// This is also the JSONL file order — the authoritative append order.
    pub(crate) order: Vec<EntryId>,
    /// Every cursor known to this session, and where each was last
    /// positioned.
    ///
    /// Shared, because cursors are a property of the log: two cursors over
    /// one log must both be discoverable, and a fork registered by one cursor
    /// has to be visible to the other.
    pub(crate) cursors: Vec<super::persist::CursorState>,
    /// Parent → direct children index, mirroring `parent_id` for every entry.
    ///
    /// Each entry's id appears under its parent's key, in append order. An
    /// entry with no parent (the root) is not a child of anything and so is
    /// not in this map.
    pub(crate) children: HashMap<EntryId, Vec<EntryId>>,
    /// Full content of truncated tool results, indexed by entry ID.
    ///
    /// When a tool result exceeds the budget fraction and is truncated, the
    /// full (redacted) content is stored here so it can be retrieved later via
    /// `Cursor::get_full_result`.
    pub(crate) details: HashMap<EntryId, ToolResultDetails>,
    /// Persistence state (save path, flushed count, pending resolution queue).
    pub(crate) persist: PersistState,
}

impl SessionLog {
    /// Create an empty in-memory log for the given header.
    pub(crate) fn new(header: SessionHeader) -> Self {
        Self {
            header,
            entries: HashMap::new(),
            order: Vec::new(),
            cursors: Vec::new(),
            children: HashMap::new(),
            details: HashMap::new(),
            persist: PersistState::in_memory(),
        }
    }

    /// Insert an entry, updating the append order and the children index.
    ///
    /// A duplicate id is **not** re-inserted and does not gain a second
    /// position in the order — the first insertion wins, so replaying a
    /// duplicated line cannot reorder the log.
    ///
    /// Returns the id under which the entry is stored (the existing id for a
    /// duplicate).
    pub(crate) fn insert(&mut self, entry: Entry) -> EntryId {
        let id = entry.id.clone();
        if self.entries.insert(id.clone(), entry).is_some() {
            return id;
        }
        if let Some(parent) = self.entries[&id].parent_id.clone() {
            self.children.entry(parent).or_default().push(id.clone());
        }
        self.order.push(id.clone());
        id
    }

    /// Record a cursor in the shared roster, replacing any earlier entry for
    /// the same id.
    pub(crate) fn cursors_push(&mut self, state: super::persist::CursorState) {
        if let Some(slot) = self.cursors.iter_mut().find(|c| c.id == state.id) {
            *slot = state;
        } else {
            self.cursors.push(state);
        }
    }

    /// The direct children of `id`, in append (== timestamp) order.
    ///
    /// Replaces the previous O(n) scan over every entry.
    pub(crate) fn children_of(&self, id: &EntryId) -> &[EntryId] {
        self.children.get(id).map_or(&[], Vec::as_slice)
    }
}
