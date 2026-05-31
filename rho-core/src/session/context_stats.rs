//! Context window usage statistics.
//!
//! [`ContextStats`] provides a snapshot of how full the context window is,
//! how many entries are in the active path, and how much budget remains.

/// A snapshot of context window usage.
///
/// Returned by [`Session::context_stats`](super::Session::context_stats) so
/// the REPL and TUI can display how full the context window is, how many
/// entries are in the active path, and how much budget remains for
/// conversation.
#[derive(Clone, Debug)]
pub struct ContextStats {
    /// Total context window size (tokens).
    pub context_window: usize,
    /// Tokens reserved for the model's completion.
    pub completion_reserve: usize,
    /// Estimated tokens consumed by the fitted messages (system + conversation).
    pub estimated_used: usize,
    /// Number of messages in the fitted path (after eviction).
    pub message_count: usize,
    /// Total entries in the session tree (including compacted/attached).
    pub entry_count: usize,
    /// Entries in the active leaf-to-root path.
    pub path_entry_count: usize,
}

impl ContextStats {
    /// Estimated remaining tokens in the prompt budget.
    ///
    /// This is `prompt_budget - estimated_used`. Negative values (i.e.
    /// over-budget) saturate to zero.
    pub fn estimated_remaining(&self) -> usize {
        let prompt_budget = self.context_window.saturating_sub(self.completion_reserve);
        prompt_budget.saturating_sub(self.estimated_used)
    }

    /// Context utilization as a percentage (0–100).
    ///
    /// Based on estimated usage relative to the prompt budget.
    pub fn utilization_percent(&self) -> u8 {
        let prompt_budget = self.context_window.saturating_sub(self.completion_reserve);
        if prompt_budget == 0 {
            return 100;
        }
        let pct = (self.estimated_used as u64 * 100 / prompt_budget as u64).min(100);
        #[allow(clippy::cast_possible_truncation)]
        let result = pct as u8;
        result
    }
}
