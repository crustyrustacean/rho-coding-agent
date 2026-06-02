// Session read-only accessors, convenience aliases, and persistence helpers.

use std::path::Path;

use crate::context::TokenBudget;
use crate::error::Result;
use crate::message::{ChatMessage, ContentBlock};
use crate::newtypes::{EntryId, ToolCallId};
use crate::redact::Redactor;
use crate::tool::{ToolResult, ToolResultDetails};

use super::Session;
use super::entry::{Entry, EntryPayload};
use super::estimator::TokenEstimator;
use super::header::SessionHeader;
use super::persist::{self, PersistState};

impl Session {
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

    /// The system prompt text, if one was set.
    ///
    /// Searches the entry tree for a `Message(System)` entry at the root
    /// and returns its text content.
    pub fn system_prompt(&self) -> Option<&str> {
        self.entries.values().find_map(|entry| {
            if let EntryPayload::Message(ChatMessage::System { content }) = &entry.payload {
                content.first().map(|b| {
                    let ContentBlock::Text { text } = b;
                    text.as_str()
                })
            } else {
                None
            }
        })
    }

    /// Append a tool result message, applying secret redaction and
    /// bounded resolution first.
    ///
    /// This is a convenience alias for [`append_tool_result`](Session::append_tool_result).
    /// It always applies redaction — there is no way to bypass the
    /// redactor through this API.
    ///
    /// If the result is truncated, the full content is stored in the
    /// session's details store and can be retrieved via
    /// [`get_full_result`](Session::get_full_result).
    ///
    /// Returns the [`EntryId`] of the new entry and the
    /// [`ToolResultDetails`] indicating whether truncation occurred.
    pub fn add_tool_result(
        &mut self,
        id: ToolCallId,
        result: &ToolResult,
    ) -> (EntryId, ToolResultDetails) {
        self.append_tool_result(id, result)
    }

    /// Retrieve the full (un-truncated) content of a tool result.
    ///
    /// Returns `Some(&ToolResultDetails)` if the entry was truncated and
    /// its full content was preserved; `None` if the entry was not
    /// truncated or does not exist.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (entry_id, details) = session.append_tool_result(call_id, &result);
    /// // Later, retrieve the full content:
    /// if let Some(full) = session.get_full_result(&entry_id) {
    ///     // `full` is the ToolResultDetails::FullOutput variant
    /// }
    /// ```
    pub fn get_full_result(&self, entry_id: &EntryId) -> Option<&ToolResultDetails> {
        self.details_store.get(entry_id)
    }

    /// The token budget.
    pub fn token_budget(&self) -> TokenBudget {
        self.token_budget
    }

    /// Estimate the token overhead of the system prompt.
    ///
    /// Searches the entry tree for the root `System` message and estimates
    /// its token count using the calibrated estimator. Returns 0 if no
    /// system prompt was set.
    pub fn system_overhead(&self) -> usize {
        self.system_prompt()
            .map_or(0, |text| self.estimator.estimate(&self.model, text))
    }

    /// Estimate the token overhead of the tool schemas.
    ///
    /// Tool schemas are sent with every request but are not part of the
    /// message history. This returns their estimated token count.
    pub fn schema_overhead(&self) -> usize {
        if let Some(cached) = self.schema_overhead_cache.get() {
            return cached;
        }
        let computed =
            crate::context::estimate_tool_schema_overhead(&self.tools, self.estimator.as_ref());
        self.schema_overhead_cache.set(Some(computed));
        computed
    }

    /// The token budget available for conversation messages after
    /// subtracting system prompt and tool-schema overhead.
    ///
    /// This is a convenience wrapper around
    /// [`TokenBudget::message_budget`](crate::TokenBudget::message_budget)
    /// that uses the session's measured overheads.
    pub fn message_budget(&self) -> usize {
        self.token_budget
            .message_budget(self.system_overhead(), self.schema_overhead())
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

    /// The path where this session would persist, or `None` for in-memory mode.
    ///
    /// This is `Some(path)` for sessions created with [`Session::new`] and
    /// `None` for sessions created with [`Session::in_memory`].
    pub fn save_path(&self) -> Option<&Path> {
        self.persist.save_path.as_deref()
    }

    /// Flush unwritten entries to the JSONL file.
    ///
    /// Appends all entries that haven't been written yet. Creates the file
    /// and its parent directories if they don't exist.
    ///
    /// For in-memory sessions, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the file cannot be opened or a write fails.
    pub fn flush(&mut self) -> Result<()> {
        persist::flush_session(self)
    }

    /// Read-only access to the persist state.
    pub(crate) fn persist_state(&self) -> &PersistState {
        &self.persist
    }

    /// Update the flushed count after a successful flush.
    pub(crate) fn set_flushed_count(&mut self, count: usize) {
        self.persist.flushed_count = count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TokenBudget;
    use crate::newtypes::EntryId;
    use crate::tool::{ToolResult, ToolResultDetails};

    // ── Overhead measurement tests ──────────────────────────────────────

    #[test]
    fn system_overhead_returns_nonzero_for_session_with_prompt() {
        let session = Session::in_memory("m", Some("You are a helpful assistant."), vec![], "/tmp");
        let overhead = session.system_overhead();
        assert!(
            overhead > 0,
            "system overhead should be > 0 for a non-empty prompt"
        );
    }

    #[test]
    fn system_overhead_returns_zero_without_prompt() {
        let session = Session::in_memory("m", None, vec![], "/tmp");
        assert_eq!(session.system_overhead(), 0);
    }

    #[test]
    fn schema_overhead_returns_zero_without_tools() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        assert_eq!(session.schema_overhead(), 0);
    }

    #[test]
    fn schema_overhead_returns_nonzero_with_tools() {
        let tools = vec![rho_ai::ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        )];
        let session = Session::in_memory("m", Some("sys"), tools, "/tmp");
        assert!(
            session.schema_overhead() > 0,
            "schema overhead should be > 0 when tools are registered"
        );
    }

    #[test]
    fn message_budget_is_prompt_minus_overheads() {
        let tools = vec![rho_ai::ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({"type": "object"}),
        )];
        let session = Session::in_memory("m", Some("sys"), tools, "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        let budget = session.token_budget();
        let expected = budget
            .prompt_budget()
            .saturating_sub(session.system_overhead())
            .saturating_sub(session.schema_overhead());
        assert_eq!(session.message_budget(), expected);
        assert!(
            session.message_budget() < budget.prompt_budget(),
            "message budget should be less than prompt budget when overhead exists"
        );
    }

    #[test]
    fn message_budget_without_tools_or_prompt() {
        let session = Session::in_memory("m", None, vec![], "/tmp")
            .with_token_budget(TokenBudget::new(10_000));
        assert_eq!(
            session.message_budget(),
            session.token_budget().prompt_budget()
        );
    }

    #[test]
    fn schema_overhead_cache_invalidated_on_set_tools() {
        let tools = vec![rho_ai::ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        )];
        let mut session = Session::in_memory("m", Some("sys"), tools, "/tmp");

        // First call computes and caches.
        let overhead_with_tools = session.schema_overhead();
        assert!(overhead_with_tools > 0, "should have overhead with tools");

        // Second call returns the cached value.
        assert_eq!(session.schema_overhead(), overhead_with_tools);

        // Replace tools with empty set — cache must be invalidated.
        session.set_tools(vec![]);
        assert_eq!(
            session.schema_overhead(),
            0,
            "overhead should be 0 after tools are cleared"
        );
    }

    // ── Details store tests ─────────────────────────────────────────────

    #[test]
    fn get_full_result_returns_none_for_non_truncated_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        let result = ToolResult::success("small output");
        let (id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

        assert_eq!(details, ToolResultDetails::None);
        assert!(
            session.get_full_result(&id).is_none(),
            "non-truncated entries should not be in the details store"
        );
    }

    #[test]
    fn get_full_result_returns_full_content_for_truncated_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10));

        let huge_content = "x".repeat(2000);
        let result = ToolResult::success(&huge_content);
        let (id, details) = session.append_tool_result(ToolCallId::from("call_1"), &result);

        assert!(
            matches!(details, ToolResultDetails::FullOutput { .. }),
            "expected FullOutput details"
        );

        let stored = session
            .get_full_result(&id)
            .expect("truncated entry should be in the details store");
        if let ToolResultDetails::FullOutput {
            original_size,
            content,
        } = stored
        {
            assert_eq!(*original_size, huge_content.len());
            assert_eq!(content.len(), huge_content.len());
        } else {
            panic!("expected FullOutput in details store");
        }
    }

    #[test]
    fn get_full_result_returns_none_for_nonexistent_entry() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        assert!(
            session.get_full_result(&fake_id).is_none(),
            "nonexistent entry should return None"
        );
    }

    #[test]
    fn details_store_independent_per_entry() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::with_reserve(100, 10));

        let huge = ToolResult::success("a".repeat(2000));
        let (id1, _) = session.append_tool_result(ToolCallId::from("call_1"), &huge);

        let small = ToolResult::success("tiny");
        let (id2, _) = session.append_tool_result(ToolCallId::from("call_2"), &small);

        let huge2 = ToolResult::success("b".repeat(3000));
        let (id3, _) = session.append_tool_result(ToolCallId::from("call_3"), &huge2);

        assert!(
            session.get_full_result(&id1).is_some(),
            "first should be stored"
        );
        assert!(
            session.get_full_result(&id2).is_none(),
            "second should not be stored"
        );
        assert!(
            session.get_full_result(&id3).is_some(),
            "third should be stored"
        );

        if let Some(ToolResultDetails::FullOutput { content, .. }) = session.get_full_result(&id1) {
            assert!(content.starts_with('a'));
        }
        if let Some(ToolResultDetails::FullOutput { content, .. }) = session.get_full_result(&id3) {
            assert!(content.starts_with('b'));
        }
    }
}
