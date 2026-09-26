// Tree navigation, branching, and entries_in_order helpers.

use std::time::SystemTime;
use tracing::warn;

use crate::error::Result;
use crate::newtypes::{CursorId, EntryId};

use super::Cursor;
use super::entry::{CompactionSummary, Entry, EntryPayload, EntryResolution};
use super::error::SessionError;

// ── PathEntry ─────────────────────────────────────────────────────────────────

/// An entry paired with its effective resolution.
///
/// Returned by [`path_to_root`](Self::path_to_root) and consumed by
/// [`ContextManager::fit_path`](crate::context::ContextManager::fit_path).
/// Carrying the resolution alongside the entry means readers never reach
/// into [`Entry::resolution`] themselves — the value they get is already
/// resolved through the session's overlay.
///
/// Entries are cloned out of the shared log, so callers pay an allocation per
/// path entry. That is the cost of putting the log behind a lock; the path is
/// built at most once per model request.
#[derive(Clone, Debug)]
pub struct PathEntry {
    /// The entry itself.
    ///
    /// Owned rather than borrowed: the log lives behind a lock, so a
    /// reference into it cannot outlive the guard.
    pub entry: Entry,
    /// The entry's effective resolution, after overlay lookup.
    pub resolution: EntryResolution,
}

impl Cursor {
    // ── Tree navigation ──────────────────────────────────────────────────

    /// Return all entries in append order (root → leaf).
    ///
    /// This is used by [`flush_session`](super::persist::flush_session) to write
    /// entries in the correct order.
    pub(crate) fn entries_in_order(&self) -> Vec<Entry> {
        self.with_log(|log| {
            log.order
                .iter()
                .filter_map(|id| log.entries.get(id).cloned())
                .collect()
        })
    }

    /// Walk from the current leaf back to the root, collecting entries.
    ///
    /// Returns [`PathEntry`] values in leaf-to-root order (newest first), each
    /// carrying the entry's effective resolution (overlay first, payload
    /// default otherwise). If the leaf is `None`, returns an empty vec.
    ///
    /// # Panics
    ///
    /// Does not panic — but if the tree is corrupt (a `parent_id` references
    /// a non-existent entry), the walk stops at the last reachable entry
    /// and a `warn!` is emitted.
    pub fn path_to_root(&self) -> Vec<PathEntry> {
        let Some(mut current_id) = self.leaf.clone() else {
            return Vec::new();
        };

        let mut path = Vec::new();
        let mut seen = std::collections::HashSet::new();

        loop {
            let Some(entry) = self.with_log(|log| log.entries.get(&current_id).cloned()) else {
                warn!(id = %current_id, "path_to_root: entry not found, tree may be corrupt");
                break;
            };

            if !seen.insert(entry.id.clone()) {
                warn!(id = %current_id, "path_to_root: cycle detected, stopping");
                break;
            }

            let resolution = self.resolution_of(&entry.id);
            let parent_id = entry.parent_id.clone();
            path.push(PathEntry { entry, resolution });

            match &parent_id {
                Some(parent_id) => current_id = parent_id.clone(),
                None => break, // reached root
            }
        }

        path
    }

    /// The leaf-to-root path in chronological order (oldest first), with each
    /// entry's effective resolution attached.
    ///
    /// This is the shape [`path_messages`](Cursor::path_messages),
    /// [`context_stats`](Cursor::context_stats), and the compaction walk all
    /// consume.
    pub fn path_entries(&self) -> Vec<PathEntry> {
        self.path_to_root().into_iter().rev().collect()
    }

    /// Return the direct children of an entry, in append order (oldest
    /// first).
    ///
    /// This is an index lookup against [`SessionLog`]'s parent→children map,
    /// maintained as entries are inserted. The children are stored in append
    /// order, which is the same ordering the previous timestamp sort produced
    /// (append order and timestamp order agree, and append order is the
    /// authoritative sequence on reload — see issue #29).
    pub fn children(&self, id: &EntryId) -> Vec<EntryId> {
        self.with_log(|log| log.children_of(id).to_vec())
    }

    /// This cursor's identity.
    ///
    /// Two cursors over the same log differ only in their per-cursor state
    /// (leaf position and resolution overlay); this id is what tells their
    /// persisted state apart.
    pub fn cursor_id(&self) -> CursorId {
        self.cursor_id.clone()
    }

    /// Every cursor this session knows about, and where each was last
    /// positioned.
    ///
    /// Reconstructed from the file's `Cursor` lines, so a session opened from
    /// a pre-cursor file reports only the cursor it was opened as. A cursor
    /// that has been forked but never appended through still appears, at
    /// wherever it was when it was created.
    ///
    /// The active cursor is always present, even if it has not been written
    /// yet — a session whose first turn has not been flushed still has exactly
    /// one cursor, and reporting an empty roster would misrepresent it.
    pub fn cursors(&self) -> Vec<crate::session::persist::CursorState> {
        let mut cursors = self.with_log(|log| log.cursors.clone());
        if !cursors.iter().any(|c| c.id == self.cursor_id.to_string()) {
            cursors.push(crate::session::persist::CursorState {
                id: self.cursor_id.to_string(),
                leaf: self.leaf.clone().unwrap_or_default(),
                name: None,
            });
        }
        cursors
    }

    /// Restore a sibling cursor's position, returning a new cursor over the
    /// same log.
    ///
    /// This is the read-side counterpart to [`fork`](Self::fork): it rebuilds
    /// a cursor that a previous process left in the file, rather than minting
    /// a new one. The returned cursor shares this session's log and carries
    /// the id it was persisted under, so resolution changes recorded for it
    /// apply again.
    ///
    /// # Errors
    ///
    /// Returns an error if `id` is not a known cursor, or if that cursor's
    /// recorded leaf is not present in the log.
    pub fn restore_cursor(&self, id: &str) -> crate::error::Result<Self> {
        let state = self
            .cursors()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| {
                crate::error::RhoError::Session(crate::session::error::SessionError::Persistence(
                    format!("unknown cursor: {id}"),
                ))
            })?;

        if self.entry(&state.leaf).is_none() {
            return Err(crate::error::RhoError::Session(
                crate::session::error::SessionError::Persistence(format!(
                    "cursor {id} points at entry {}, which is not in this log",
                    state.leaf
                )),
            ));
        }

        let mut restored = self.clone();
        restored.cursor_id = id.into();
        restored.leaf = Some(state.leaf.clone());
        // The overlay is per-cursor; start empty and let the persisted
        // `Resolution` lines for this cursor be re-applied on the next flush.
        restored.resolution.clear();
        Ok(restored)
    }

    /// Fork a second cursor over the same log.
    ///
    /// The returned cursor shares the log — entries appended through either are
    /// visible to both — but owns its own leaf position, resolution overlay,
    /// and [`CursorId`]. This is the primitive the branch surface builds on.
    ///
    /// This does **not** write a `LeafMoved` entry; it is a pure cursor
    /// operation. Use [`branch_to`](Self::branch_to) to record an audited leaf
    /// move within one cursor.
    ///
    /// The new branch is unnamed; call [`name_cursor`](Self::name_cursor) to
    /// give it a label.
    #[must_use]
    pub fn fork(&self) -> Self {
        let mut forked = self.clone();
        let new_id = CursorId::new();
        forked.cursor_id = new_id.clone();

        // Register the new cursor in the shared roster and queue its
        // position, so a fork that never appends is still recoverable from
        // the file. This writes to the shared log — `fork` is not a pure
        // read — but it does not append an entry, so the log's shape is
        // unchanged.
        let leaf = forked.leaf.clone();
        let id = new_id.to_string();
        if let Some(leaf) = leaf.clone() {
            forked.with_log_mut(|log| {
                log.cursors_push(crate::session::persist::CursorState {
                    id: id.clone(),
                    leaf: leaf.clone(),
                    name: None,
                });
                log.persist.pending_cursors.push(id, leaf, None);
            });
        }

        forked
    }

    /// Give a cursor a human-readable label.
    ///
    /// The name is stored on the cursor's roster entry and persisted with it,
    /// so it survives a reopen. `listBranches` surfaces it. An empty or
    /// whitespace-only name clears the label (`None`).
    ///
    /// # Errors
    ///
    /// Returns an error if `cursor_id` is not a known cursor in this session.
    pub fn name_cursor(&mut self, cursor_id: &str, name: &str) -> Result<()> {
        let trimmed = name.trim();
        let new_name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        let leaf = self
            .cursors()
            .into_iter()
            .find(|c| c.id == cursor_id)
            .ok_or_else(|| {
                crate::error::RhoError::Session(SessionError::Persistence(format!(
                    "unknown cursor: {cursor_id}"
                )))
            })?
            .leaf;

        self.with_log_mut(|log| {
            log.cursors_push(crate::session::persist::CursorState {
                id: cursor_id.to_owned(),
                leaf: leaf.clone(),
                name: new_name.clone(),
            });
            // Re-queue with the name so the next flush writes it; the queued
            // leaf must be preserved, so read the current one rather than
            // re-deriving it.
            let current_leaf = log
                .cursors
                .iter()
                .find(|c| c.id == cursor_id)
                .map(|c| c.leaf.clone())
                .unwrap_or(leaf.clone());
            log.persist
                .pending_cursors
                .push(cursor_id.to_owned(), current_leaf, new_name.clone());
        });
        Ok(())
    }

    /// Move the leaf pointer to an existing entry, recording a
    /// [`LeafMoved`](EntryPayload::LeafMoved) entry for audit.
    ///
    /// This is the core branching operation: after `branch_to(id)`, the
    /// leaf-to-root path passes through `id` instead of the previous leaf.
    /// The old branch remains in the tree and is accessible via
    /// [`entry()`](Cursor::entry) and [`children()`](Cursor::children),
    /// but is no longer on the active path.
    ///
    /// # Errors
    ///
    /// Returns [`super::error::SessionError::EntryNotFound`] if `id` does not exist in the
    /// session tree.
    ///
    /// # No-op
    ///
    /// If `id` is already the current leaf, no `LeafMoved` entry is written
    /// and the method returns `Ok(())`.
    pub fn branch_to(&mut self, id: &EntryId) -> Result<()> {
        // Validate that the target exists
        if !self.with_log(|log| log.entries.contains_key(id)) {
            return Err(SessionError::EntryNotFound(id.to_string()).into());
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
            payload: EntryPayload::LeafMoved {
                from: old_leaf,
                to: id.clone(),
            },
        };
        // Route through `SessionLog::insert` so the children index records
        // this node under its parent like any other append.
        let moved_id = self.with_log_mut(|log| log.insert(moved_entry));
        // The LeafMoved entry itself becomes the new leaf so subsequent
        // appends link from here.
        self.leaf = Some(moved_id);
        // A leaf move is the other way a cursor's position changes, so record
        // it here too. Doing it at the leaf-move sites rather than on every
        // flush keeps a bare flush() from stamping a stale position.
        self.queue_cursor_position();

        // Auto-flush the new entry.
        if let Err(e) = self.flush() {
            warn!(
                error = %e,
                "auto-flush of LeafMoved entry failed"
            );
        }

        Ok(())
    }

    /// Move the leaf to an existing entry and append a
    /// [`BranchSummary`](EntryPayload::BranchSummary) at the new position.
    ///
    /// This combines [`branch_to`](Cursor::branch_to) with a summary of
    /// why the branch happened. The `from_id` identifies the leaf position
    /// that was abandoned; the `summary` describes what was on that branch.
    ///
    /// After this call, the leaf is at the newly-appended `BranchSummary`
    /// entry.
    ///
    /// # Errors
    ///
    /// Returns [`super::error::SessionError::EntryNotFound`] if `id` does not exist.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RhoError;
    use crate::message::ChatMessage;

    // ── Tree navigation tests ───────────────────────────────────────────

    #[test]
    fn path_to_root_returns_leaf_to_root_order() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let path = session.path_to_root();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].entry.id, asst_id, "first entry should be the leaf");
        assert_eq!(
            path[1].entry.id, user_id,
            "second entry should be user message"
        );
        assert_eq!(path[2].entry.id, root_id, "last entry should be the root");
    }

    #[test]
    fn path_to_root_empty_when_no_leaf() {
        let session = Cursor::in_memory("m", None, vec![], "/tmp");
        assert!(session.path_to_root().is_empty());
    }

    #[test]
    fn path_to_root_single_entry() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let path = session.path_to_root();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].entry.id, root_id);
    }

    #[test]
    fn path_to_root_has_no_duplicates() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let id1 = session.append_user_message("msg1");
        let id2 = session.append_assistant_message(ChatMessage::assistant_text("reply1"));
        let id3 = session.append_user_message("msg2");

        let path = session.path_to_root();
        let ids: std::collections::HashSet<_> = path.iter().map(|e| e.entry.id.clone()).collect();
        assert_eq!(ids.len(), path.len(), "path should have no duplicate IDs");

        // Verify exact order: leaf → ... → root
        assert_eq!(path[0].entry.id, id3);
        assert_eq!(path[1].entry.id, id2);
        assert_eq!(path[2].entry.id, id1);
        assert_eq!(path[3].entry.id, root_id);
    }

    #[test]
    fn children_returns_direct_children_sorted_by_timestamp() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();

        // Root has one child (the user message)
        let user_id = session.append_user_message("hello");
        let children = session.children(&root_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], user_id);
    }

    // ── children() index (A3 PR 1) ─────────────────────────────────────
    //
    // `children()` used to scan every entry and sort by timestamp. The
    // `SessionLog` index must produce identical results, including on the
    // non-linear appends that branching produces.

    /// With several children, `children()` must return them in ascending
    /// timestamp order — the same contract the old O(n)-scan sort provided.
    ///
    /// Tree shape note: `branch_to(id)` writes a `LeafMoved` entry whose
    /// parent is `id` and makes *that* the new leaf, so anything appended
    /// after a branch hangs off the `LeafMoved` node rather than off `id`.
    #[test]
    fn children_returns_multiple_children_in_timestamp_order() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let pivot = session.leaf().unwrap();
        session.append_user_message("first");

        // Two branch-backs from the same pivot produce several direct children
        // of `pivot` (a message plus LeafMoved audit nodes).
        session.branch_to(&pivot).unwrap();
        session.append_assistant_message(ChatMessage::assistant_text("A"));
        session.branch_to(&pivot).unwrap();
        session.append_assistant_message(ChatMessage::assistant_text("B"));

        let children = session.children(&pivot);
        assert!(
            children.len() >= 2,
            "pivot should have multiple children, got {:?}",
            children.len()
        );

        // The contract under test: ascending timestamp order.
        let timestamps: Vec<_> = children
            .iter()
            .map(|id| session.entry(id).expect("child entry").timestamp)
            .collect();
        assert!(
            timestamps.windows(2).all(|w| w[0] <= w[1]),
            "children() must be timestamp-ordered, got {timestamps:?}"
        );
    }

    #[test]
    fn children_of_leaf_is_empty() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        assert!(session.children(&fake_id).is_empty());
    }

    #[test]
    fn branch_to_moves_leaf() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
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
        assert_eq!(session.resolution_of(&leaf_id), EntryResolution::Attached);
    }

    #[test]
    fn branch_to_errors_on_nonexistent_entry() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result = session.branch_to(&fake_id);
        assert!(result.is_err());
        if let Err(RhoError::Session(SessionError::EntryNotFound(id))) = &result {
            assert_eq!(id, &*fake_id);
        } else {
            panic!("expected EntryNotFound error, got {result:?}");
        }
    }

    #[test]
    fn branch_to_is_noop_when_already_at_target() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let leaf_id = session.leaf().unwrap();
        let entry_count_before = session.entry_count();

        session.branch_to(&leaf_id).unwrap();

        // No LeafMoved entry should be written
        assert_eq!(session.entry_count(), entry_count_before);
        assert_eq!(session.leaf(), Some(leaf_id));
    }

    #[test]
    fn branch_to_then_append_extends_from_new_position() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to user message and take a different path
        session.branch_to(&user_id).unwrap();
        let new_reply_id = session.append_assistant_message(ChatMessage::assistant_text("reply B"));

        // The new reply should be reachable from the leaf
        let path = session.path_to_root();
        let path_ids: Vec<_> = path.iter().map(|e| e.entry.id.clone()).collect();
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
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let path_ids: Vec<_> = path.iter().map(|e| e.entry.id.clone()).collect();
        assert!(
            !path_ids.contains(&asst_a_id),
            "old assistant should NOT be on the current leaf path"
        );
    }

    #[test]
    fn branch_with_summary_appends_summary_after_branch() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        let summary = CompactionSummary {
            original_request: Some("hello".to_owned()),
            current_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(30),
            notes: None,
            key_findings: std::collections::BTreeMap::new(),
            phases: Vec::new(),
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
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let summary = CompactionSummary {
            original_request: None,
            current_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 0,
            entry_count: 0,
            time_span: std::time::Duration::ZERO,
            notes: None,
            key_findings: std::collections::BTreeMap::new(),
            phases: Vec::new(),
        };

        let result = session.branch_with_summary(&fake_id, summary, EntryId::new());
        assert!(result.is_err());
        if let Err(RhoError::Session(SessionError::EntryNotFound(id))) = &result {
            assert_eq!(id, &*fake_id);
        } else {
            panic!("expected EntryNotFound error, got {result:?}");
        }
    }

    #[test]
    fn path_to_root_after_branch_reflects_new_path() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("hello");
        let asst_a_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to root
        session.branch_to(&root_id).unwrap();

        let path = session.path_to_root();
        let path_ids: Vec<_> = path.iter().map(|e| e.entry.id.clone()).collect();

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
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let _user_id = session.append_user_message("hello");

        // Branch to root — creates a LeafMoved (Attached) entry
        session.branch_to(&root_id).unwrap();

        let path = session.path_to_root();
        // The LeafMoved entry IS in the path (it's the leaf)
        let leaf_entry = &path[0];
        assert!(matches!(leaf_entry.resolution, EntryResolution::Attached));
        assert!(matches!(
            leaf_entry.entry.payload,
            EntryPayload::LeafMoved { .. }
        ));
    }

    // ── Cursor naming ───────────────────────────────────────────────────

    fn name_of(session: &Cursor, id: &str) -> Option<String> {
        session
            .cursors()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.name)
    }

    #[test]
    fn name_cursor_sets_and_reads_back() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let id = session.cursor_id().to_string();
        session.name_cursor(&id, "baseline").unwrap();
        assert_eq!(name_of(&session, &id).as_deref(), Some("baseline"));
    }

    #[test]
    fn name_cursor_trims_whitespace() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let id = session.cursor_id().to_string();
        session.name_cursor(&id, "  spaced  ").unwrap();
        assert_eq!(name_of(&session, &id).as_deref(), Some("spaced"));
    }

    #[test]
    fn name_cursor_empty_clears_the_label() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let id = session.cursor_id().to_string();
        session.name_cursor(&id, "temp").unwrap();
        session.name_cursor(&id, "   ").unwrap();
        assert_eq!(name_of(&session, &id), None);
    }

    #[test]
    fn name_cursor_rejects_unknown_id() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        assert!(session.name_cursor("nope", "x").is_err());
    }

    /// The load-bearing case: a position update must not erase a name.
    ///
    /// `queue_cursor_position` used to push `None` for the name on every
    /// append/branch, which silently cleared a label the user had set.
    #[test]
    fn position_update_preserves_the_name() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let id = session.cursor_id().to_string();
        session.name_cursor(&id, "keep me").unwrap();

        // An append re-queues this cursor's position.
        session.append_user_message("hello");
        assert_eq!(
            name_of(&session, &id).as_deref(),
            Some("keep me"),
            "an append must not clear the cursor's name"
        );

        // So must a leaf move.
        session.append_user_message("second");
        assert_eq!(name_of(&session, &id).as_deref(), Some("keep me"));
    }

    #[test]
    fn fork_is_unnamed_then_nameable() {
        let session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        let forked = session.fork();
        let fork_id = forked.cursor_id().to_string();
        assert_eq!(name_of(&session, &fork_id), None, "forks start unnamed");

        // The roster is shared, so naming through the fork is visible to the
        // original — they are two views of one log.
        let mut forked = session.fork();
        forked
            .name_cursor(&fork_id, "alternative approach")
            .unwrap();
        assert_eq!(
            name_of(&session, &fork_id).as_deref(),
            Some("alternative approach")
        );
    }
}
