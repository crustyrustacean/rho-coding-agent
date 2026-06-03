// Tree navigation, branching, and entries_in_order helpers.

use std::time::SystemTime;
use tracing::warn;

use crate::error::Result;
use crate::newtypes::EntryId;

use super::Session;
use super::entry::{CompactionSummary, Entry, EntryPayload, EntryResolution};
use super::error::SessionError;

impl Session {
    // ── Tree navigation ──────────────────────────────────────────────────

    /// Return all entries in append order (root → leaf).
    ///
    /// This is used by [`flush_session`](super::persist::flush_session) to write
    /// entries in the correct order.
    pub(crate) fn entries_in_order(&self) -> Vec<Entry> {
        self.append_order
            .iter()
            .filter_map(|id| self.entries.get(id).cloned())
            .collect()
    }

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
            resolution: EntryResolution::Attached,
            payload: EntryPayload::LeafMoved {
                from: old_leaf,
                to: id.clone(),
            },
        };
        let moved_id = moved_entry.id.clone();
        self.entries.insert(moved_id.clone(), moved_entry);
        self.append_order.push(moved_id.clone());
        // The LeafMoved entry itself becomes the new leaf so subsequent
        // appends link from here.
        self.leaf = Some(moved_id);

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RhoError;
    use crate::message::ChatMessage;

    // ── Tree navigation tests ───────────────────────────────────────────

    #[test]
    fn path_to_root_returns_leaf_to_root_order() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let session = Session::in_memory("m", None, vec![], "/tmp");
        assert!(session.path_to_root().is_empty());
    }

    #[test]
    fn path_to_root_single_entry() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let path = session.path_to_root();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].id, root_id);
    }

    #[test]
    fn path_to_root_has_no_duplicates() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();

        // Root has one child (the user message)
        let user_id = session.append_user_message("hello");
        let children = session.children(&root_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0], user_id);
    }

    #[test]
    fn children_of_leaf_is_empty() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        assert!(session.children(&fake_id).is_empty());
    }

    #[test]
    fn branch_to_moves_leaf() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let leaf_id = session.leaf().unwrap();
        let entry_count_before = session.entry_count();

        session.branch_to(&leaf_id).unwrap();

        // No LeafMoved entry should be written
        assert_eq!(session.entry_count(), entry_count_before);
        assert_eq!(session.leaf(), Some(leaf_id));
    }

    #[test]
    fn branch_to_then_append_extends_from_new_position() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
            key_findings: std::collections::BTreeMap::new(),
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 0,
            entry_count: 0,
            time_span: std::time::Duration::ZERO,
            notes: None,
            key_findings: std::collections::BTreeMap::new(),
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
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
}
