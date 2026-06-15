//! Context window usage statistics and API token accumulation.
//!
//! [`ContextStats`] provides a snapshot of how full the context window is.
//! [`ApiUsage`] tracks cumulative token counts across all LLM requests.
//!
//! [`ContextStats`] provides a snapshot of how full the context window is,
//! how many entries are in the active path, and how much budget remains.
//!
//! Enhanced stats include token distribution by message role, entry resolution,
//! and session phase, giving the agent and user better insight into what's
//! consuming the context budget.

/// Token distribution by message role.
#[derive(Clone, Debug, Default)]
pub struct RoleTokenDistribution {
    /// Tokens consumed by system messages.
    pub system: usize,
    /// Tokens consumed by user messages.
    pub user: usize,
    /// Tokens consumed by assistant messages (text + tool-call overhead).
    pub assistant: usize,
    /// Tokens consumed by tool result messages.
    pub tool: usize,
}

impl RoleTokenDistribution {
    /// Total tokens across all roles.
    pub fn total(&self) -> usize {
        self.system + self.user + self.assistant + self.tool
    }
}

/// Token distribution by entry resolution level.
#[derive(Clone, Debug, Default)]
pub struct ResolutionTokenDistribution {
    /// Tokens from entries at Full resolution.
    pub full: usize,
    /// Tokens from entries at Outlined resolution.
    pub outlined: usize,
    /// Tokens from entries at Summarized resolution.
    pub summarized: usize,
    /// Tokens from entries at Pinned resolution (rendered as Full).
    pub pinned: usize,
}

impl ResolutionTokenDistribution {
    /// Total tokens across all resolution levels.
    pub fn total(&self) -> usize {
        self.full + self.outlined + self.summarized + self.pinned
    }
}

/// Token distribution by session phase.
///
/// Only populated when the session has phase-tracking information.
/// Phases with zero token contribution are omitted.
#[derive(Clone, Debug, Default)]
pub struct PhaseTokenDistribution {
    /// Tokens in the Exploration phase.
    pub exploration: usize,
    /// Tokens in the Execution phase.
    pub execution: usize,
    /// Tokens in the Verification phase.
    pub verification: usize,
    /// Tokens in the Conclusion phase.
    pub conclusion: usize,
    /// Tokens from entries with no phase classification (e.g., system message).
    pub unclassified: usize,
}

impl PhaseTokenDistribution {
    /// Total tokens across all phases.
    pub fn total(&self) -> usize {
        self.exploration + self.execution + self.verification + self.conclusion + self.unclassified
    }
}

/// Cumulative token usage across all LLM requests in a session.
///
/// Accumulated from each [`rho_ai::StreamUsage`] reported by the model.
/// Unlike [`ContextStats`] (which estimates tokens from message content),
/// these are the actual token counts reported by the provider.
#[derive(Clone, Debug, Default)]
pub struct ApiUsage {
    /// Total input (prompt) tokens across all requests.
    pub total_input_tokens: u64,
    /// Total output (completion) tokens across all requests.
    pub total_output_tokens: u64,
    /// Cumulative cost in USD, if the provider reports it.
    pub total_cost: f64,
    /// Number of LLM requests made.
    pub request_count: u32,
}

impl ApiUsage {
    /// Accumulate a single response's usage into the running totals.
    pub fn accumulate(&mut self, usage: &rho_ai::StreamUsage) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        if let Some(cost) = usage.cost {
            self.total_cost += cost;
        }
        self.request_count += 1;
    }

    /// Total tokens (input + output).
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }
}

/// A snapshot of context window usage.
///
/// Returned by [`Session::context_stats`](super::Session::context_stats) so
/// the REPL and TUI can display how full the context window is, how many
/// entries are in the active leaf-to-root path, and how much budget remains.
#[derive(Clone, Debug, Default)]
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
    /// Token distribution by message role.
    pub role_tokens: RoleTokenDistribution,
    /// Token distribution by entry resolution level.
    pub resolution_tokens: ResolutionTokenDistribution,
    /// Token distribution by session phase.
    pub phase_tokens: PhaseTokenDistribution,
    /// Tokens consumed by compaction summary entries.
    pub compaction_tokens: usize,
    /// Number of entries that have been compacted (resolution = Compacted).
    pub compacted_entry_count: usize,
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

    /// The prompt budget (context window minus completion reserve).
    pub fn prompt_budget(&self) -> usize {
        self.context_window.saturating_sub(self.completion_reserve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RoleTokenDistribution tests ──────────────────────────────────────

    #[test]
    fn role_distribution_total() {
        let dist = RoleTokenDistribution {
            system: 100,
            user: 200,
            assistant: 50,
            tool: 300,
        };
        assert_eq!(dist.total(), 650);
    }

    #[test]
    fn role_distribution_default_is_zero() {
        let dist = RoleTokenDistribution::default();
        assert_eq!(dist.total(), 0);
    }

    // ── ResolutionTokenDistribution tests ────────────────────────────────

    #[test]
    fn resolution_distribution_total() {
        let dist = ResolutionTokenDistribution {
            full: 1000,
            outlined: 200,
            summarized: 50,
            pinned: 150,
        };
        assert_eq!(dist.total(), 1400);
    }

    #[test]
    fn resolution_distribution_default_is_zero() {
        let dist = ResolutionTokenDistribution::default();
        assert_eq!(dist.total(), 0);
    }

    // ── PhaseTokenDistribution tests ──────────────────────────────────────

    #[test]
    fn phase_distribution_total() {
        let dist = PhaseTokenDistribution {
            exploration: 500,
            execution: 1000,
            verification: 200,
            conclusion: 50,
            unclassified: 100,
        };
        assert_eq!(dist.total(), 1850);
    }

    #[test]
    fn phase_distribution_default_is_zero() {
        let dist = PhaseTokenDistribution::default();
        assert_eq!(dist.total(), 0);
    }

    // ── ContextStats tests ────────────────────────────────────────────────

    #[test]
    fn utilization_calculation() {
        let stats = ContextStats {
            context_window: 32_768,
            completion_reserve: 8_192,
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
        // prompt_budget = 32_768 - 8_192 = 24_576
        // utilization = 12_288 / 24_576 = 50%
        assert_eq!(stats.utilization_percent(), 50);
        // remaining = 24_576 - 12_288 = 12_288
        assert_eq!(stats.estimated_remaining(), 12_288);
    }

    #[test]
    fn zero_budget() {
        let stats = ContextStats {
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
    fn over_budget() {
        let stats = ContextStats {
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

    #[test]
    fn prompt_budget_calculation() {
        let stats = ContextStats {
            context_window: 65_536,
            completion_reserve: 8_192,
            estimated_used: 20_000,
            message_count: 0,
            entry_count: 0,
            path_entry_count: 0,
            role_tokens: RoleTokenDistribution::default(),
            resolution_tokens: ResolutionTokenDistribution::default(),
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        assert_eq!(stats.prompt_budget(), 57_344);
    }

    #[test]
    fn role_tokens_sum_matches_used() {
        let stats = ContextStats {
            context_window: 32_768,
            completion_reserve: 8_192,
            estimated_used: 600,
            message_count: 0,
            entry_count: 0,
            path_entry_count: 0,
            role_tokens: RoleTokenDistribution {
                system: 200,
                user: 100,
                assistant: 50,
                tool: 250,
            },
            resolution_tokens: ResolutionTokenDistribution::default(),
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        assert_eq!(stats.role_tokens.total(), 600);
        assert_eq!(stats.role_tokens.total(), stats.estimated_used);
    }

    #[test]
    fn resolution_tokens_sum_matches_used() {
        let stats = ContextStats {
            context_window: 32_768,
            completion_reserve: 8_192,
            estimated_used: 500,
            message_count: 0,
            entry_count: 0,
            path_entry_count: 0,
            role_tokens: RoleTokenDistribution::default(),
            resolution_tokens: ResolutionTokenDistribution {
                full: 300,
                outlined: 100,
                summarized: 50,
                pinned: 50,
            },
            phase_tokens: PhaseTokenDistribution::default(),
            compaction_tokens: 0,
            compacted_entry_count: 0,
        };
        assert_eq!(stats.resolution_tokens.total(), 500);
    }

    // ── ApiUsage tests ────────────────────────────────────────────────

    #[test]
    fn api_usage_starts_at_zero() {
        let usage = ApiUsage::default();
        assert_eq!(usage.total_input_tokens, 0);
        assert_eq!(usage.total_output_tokens, 0);
        assert!((usage.total_cost).abs() < f64::EPSILON);
        assert_eq!(usage.request_count, 0);
        assert_eq!(usage.total_tokens(), 0);
    }

    #[test]
    fn api_usage_accumulates_single_response() {
        let mut usage = ApiUsage::default();
        usage.accumulate(&rho_ai::StreamUsage::new(100, 50));
        assert_eq!(usage.total_input_tokens, 100);
        assert_eq!(usage.total_output_tokens, 50);
        assert_eq!(usage.total_tokens(), 150);
        assert_eq!(usage.request_count, 1);
    }

    #[test]
    fn api_usage_accumulates_multiple_responses() {
        let mut usage = ApiUsage::default();
        usage.accumulate(&rho_ai::StreamUsage::new(1000, 200));
        usage.accumulate(&rho_ai::StreamUsage::new(1500, 300));
        usage.accumulate(&rho_ai::StreamUsage::new(2000, 100));
        assert_eq!(usage.total_input_tokens, 4500);
        assert_eq!(usage.total_output_tokens, 600);
        assert_eq!(usage.total_tokens(), 5100);
        assert_eq!(usage.request_count, 3);
    }

    #[test]
    fn api_usage_accumulates_cost() {
        let mut usage = ApiUsage::default();
        let mut u1 = rho_ai::StreamUsage::new(100, 50);
        u1.cost = Some(0.001);
        let mut u2 = rho_ai::StreamUsage::new(200, 100);
        u2.cost = Some(0.002);
        usage.accumulate(&u1);
        usage.accumulate(&u2);
        assert!((usage.total_cost - 0.003).abs() < f64::EPSILON);
    }

    #[test]
    fn api_usage_cost_defaults_to_zero() {
        let mut usage = ApiUsage::default();
        usage.accumulate(&rho_ai::StreamUsage::new(100, 50));
        assert!((usage.total_cost).abs() < f64::EPSILON);
    }
}
