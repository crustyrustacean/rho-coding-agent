// Session read-only accessors, convenience aliases, and persistence helpers.

use std::path::PathBuf;

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
    ///
    /// Returns a clone: the log is behind a lock, so a borrow cannot outlive it.
    pub fn header(&self) -> SessionHeader {
        self.with_log(|log| log.header.clone())
    }

    /// The current leaf entry ID. `None` only if the session was constructed
    /// without a system prompt (before the first append).
    pub fn leaf(&self) -> Option<EntryId> {
        self.leaf.clone()
    }

    /// Look up an entry by ID.
    ///
    /// Returns a clone: the log is behind a lock, so a borrow cannot outlive it.
    pub fn entry(&self, id: &EntryId) -> Option<Entry> {
        self.with_log(|log| log.entries.get(id).cloned())
    }

    /// The model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The system prompt text, if one was set.
    ///
    /// Searches the entry tree for a `Message(System)` entry at the root
    /// and returns its text content.
    pub fn system_prompt(&self) -> Option<String> {
        self.with_log(|log| {
            log.entries.values().find_map(|entry| {
                if let EntryPayload::Message(ChatMessage::System { content }) = &entry.payload {
                    content.first().map(|b| {
                        let ContentBlock::Text { text } = b;
                        text.clone()
                    })
                } else {
                    None
                }
            })
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
    /// Returns `Some(ToolResultDetails)` if the entry was truncated and
    /// its full content was preserved; `None` if the entry was not
    /// truncated or does not exist. The value is cloned, since the log is
    /// behind a lock.
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
    pub fn get_full_result(&self, entry_id: &EntryId) -> Option<ToolResultDetails> {
        self.with_log(|log| log.details.get(entry_id).cloned())
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
            .map_or(0, |text| self.estimator.estimate(&self.model, &text))
    }

    /// Estimate the token overhead of the tool schemas.
    ///
    /// Tool schemas are sent with every request but are not part of the
    /// message history. This returns their estimated token count.
    ///
    /// The result is memoized because recomputing serialises the entire tools
    /// array to JSON (measured ~5.1µs per call with a realistic 8-tool set),
    /// and this is consulted on every budget check.
    pub fn schema_overhead(&self) -> usize {
        // Fast path: already computed.
        if let Some(cached) = *self
            .schema_overhead_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return cached;
        }
        let computed =
            crate::context::estimate_tool_schema_overhead(&self.tools, self.estimator.as_ref());
        *self
            .schema_overhead_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(computed);
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

    /// The cached tool schemas currently advertised to the model.
    ///
    /// This snapshot is captured at session start and only refreshed when
    /// [`Session::set_tools`](crate::Session::set_tools) is called (e.g. after
    /// an extension reload or session resume). Inspecting it lets callers
    /// detect drift between the live [`ToolRegistry`](crate::ToolRegistry) and
    /// what the session will actually send to the model.
    pub fn tools(&self) -> &[rho_ai::ToolDefinition] {
        &self.tools
    }

    /// Read-only access to the redactor.
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Read-only access to the estimator.
    ///
    /// Calibration goes through this shared reference: `TokenEstimator::calibrate`
    /// takes `&self` so the estimator can be shared across cursors.
    pub fn estimator(&self) -> &dyn TokenEstimator {
        self.estimator.as_ref()
    }

    /// Total number of entries in the tree.
    pub fn entry_count(&self) -> usize {
        self.with_log(|log| log.entries.len())
    }

    /// The path where this session would persist, or `None` for in-memory mode.
    ///
    /// This is `Some(path)` for sessions created with [`Session::new`] and
    /// `None` for sessions created with [`Session::in_memory`]. Returns a
    /// clone, since the log is behind a lock.
    pub fn save_path(&self) -> Option<PathBuf> {
        self.with_log(|log| log.persist.save_path.clone())
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
    /// Returns [`crate::error::RhoError`] if the file cannot be opened or a write fails.
    pub fn flush(&mut self) -> Result<()> {
        persist::flush_session(self)
    }

    /// A clone of the persist state.
    pub(crate) fn persist_state(&self) -> PersistState {
        self.with_log(|log| log.persist.clone())
    }

    /// Cumulative API token usage across all LLM requests.
    pub fn api_usage(&self) -> &crate::session::context_stats::ApiUsage {
        &self.api_usage
    }

    /// Accumulate token usage from a single LLM response.
    pub fn accumulate_usage(&mut self, usage: &rho_ai::StreamUsage) {
        self.api_usage.accumulate(usage);
    }

    /// Update the flushed count after a successful flush.
    pub(crate) fn set_flushed_count(&mut self, count: usize) {
        self.with_log_mut(|log| log.persist.flushed_count = count);
    }

    /// Clear the queued resolution changes after a successful flush.
    pub(crate) fn clear_pending_resolution(&mut self) {
        self.with_log_mut(|log| log.persist.pending_resolution.clear());
    }

    /// Queue a resolution change for persistence.
    ///
    /// Entries can be rewritten several times before the next flush (the
    /// eviction planner emits Outline then Summarize for the same entry), so
    /// an existing queued change for the same id is replaced rather than
    /// appended. Replay is last-write-wins, so only the final value matters.
    pub(crate) fn queue_resolution(
        &mut self,
        id: EntryId,
        resolution: crate::session::entry::EntryResolution,
    ) {
        self.with_log_mut(|log| {
            if let Some(slot) = log
                .persist
                .pending_resolution
                .iter_mut()
                .find(|(pending_id, _)| pending_id == &id)
            {
                slot.1 = resolution;
            } else {
                log.persist.pending_resolution.push((id, resolution));
            }
        });
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

    // ── Sync / schema-overhead cache (A3 PR 1) ──────────────────────────
    //
    // `Session` must be `Sync` so it can go behind `Arc<Mutex<..>>` in the
    // log/cursor split (#65). The `Cell`-based schema-overhead memo made it
    // `!Sync`; the compile-time guard below stops that regressing.

    /// Compile-time assertion that `Session: Sync`.
    ///
    /// A `Cell` (or any other non-atomic interior mutability) in the struct
    /// makes this fail to compile.
    #[test]
    fn session_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<Session>();
    }

    /// The schema-overhead memo must still hit after the `Cell` → `Mutex`
    /// migration: repeated calls return the cached value without recomputing.
    ///
    /// Recomputing costs ~5.1µs (measured) because it serialises the whole
    /// tools array to JSON, so losing the memo is a real regression.
    #[test]
    fn schema_overhead_cache_still_memoises() {
        let tools = vec![rho_ai::ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        )];
        let session = Session::in_memory("m", Some("sys"), tools, "/tmp");

        let first = session.schema_overhead();
        assert!(first > 0, "overhead should be non-zero with tools present");
        // Populates the cache; every subsequent call must observe the same value.
        assert_eq!(session.schema_overhead(), first);
        assert_eq!(session.schema_overhead(), first);
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
            assert_eq!(original_size, huge_content.len());
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
