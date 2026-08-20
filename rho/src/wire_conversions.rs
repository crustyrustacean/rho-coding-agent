//! Conversions from `rho-core` domain types to `rho-protocol` wire types.
//!
//! These live in the `rho` crate (not `rho-protocol`) so the protocol crate
//! stays a pure-serde leaf with no workspace dependencies: only the server
//! produces these conversions, and only the server depends on `rho-core`.
//!
//! They are free functions rather than `From` impls because the orphan rule
//! forbids implementing a foreign trait for a foreign type — with the types
//! now split across `rho-core` (source) and `rho-protocol` (target), neither
//! side can host the impl. Callers use `session_stats_wire(&stats)` etc.

use rho_core::session::context_stats::ApiUsage;
use rho_core::{AgentResult, AgentState, LoopFinishReason, ToolCallOutcome, ToolRisk};

use rho_protocol::{
    AgentEndParams, GetSessionStatsResult, TokenUsageWire, ToolCallOutcomeWire, ToolCallRecordWire,
};

/// Build a [`GetSessionStatsResult`] from the session's context stats.
pub fn session_stats_wire(stats: &rho_core::session::ContextStats) -> GetSessionStatsResult {
    GetSessionStatsResult {
        context_window: stats.context_window as u64,
        completion_reserve: stats.completion_reserve as u64,
        estimated_used: stats.estimated_used as u64,
        estimated_remaining: stats.estimated_remaining() as u64,
        utilization_percent: u64::from(stats.utilization_percent()),
        message_count: stats.message_count as u64,
        entry_count: stats.entry_count as u64,
        path_entry_count: stats.path_entry_count as u64,
        compacted_entry_count: stats.compacted_entry_count as u64,
        compaction_tokens: stats.compaction_tokens as u64,
        role_tokens: rho_protocol::RoleTokenDist {
            system: stats.role_tokens.system as u64,
            user: stats.role_tokens.user as u64,
            assistant: stats.role_tokens.assistant as u64,
            tool: stats.role_tokens.tool as u64,
        },
        resolution_tokens: rho_protocol::ResolutionTokenDist {
            full: stats.resolution_tokens.full as u64,
            outlined: stats.resolution_tokens.outlined as u64,
            summarized: stats.resolution_tokens.summarized as u64,
            pinned: stats.resolution_tokens.pinned as u64,
        },
        phase_tokens: rho_protocol::PhaseTokenDistWire {
            exploration: stats.phase_tokens.exploration as u64,
            execution: stats.phase_tokens.execution as u64,
            verification: stats.phase_tokens.verification as u64,
            conclusion: stats.phase_tokens.conclusion as u64,
            unclassified: stats.phase_tokens.unclassified as u64,
        },
        api_usage: rho_protocol::ApiUsageWire::default(),
    }
}

/// Fill in a result's `apiUsage` field from the session's cumulative usage.
pub fn with_api_usage(
    mut result: GetSessionStatsResult,
    usage: &ApiUsage,
) -> GetSessionStatsResult {
    result.api_usage = rho_protocol::ApiUsageWire {
        total_input_tokens: usage.total_input_tokens,
        total_output_tokens: usage.total_output_tokens,
        total_cached_tokens: usage.total_cached_tokens,
        total_tokens: usage.total_tokens(),
        total_cost: usage.total_cost,
        request_count: usage.request_count,
    };
    result
}

/// Build [`AgentEndParams`] from the agent loop's structured result.
pub fn agent_end_wire(r: Box<AgentResult>) -> AgentEndParams {
    AgentEndParams {
        reply: r.reply,
        iterations: r.iterations,
        usage: TokenUsageWire {
            input_tokens: r.usage.input_tokens,
            output_tokens: r.usage.output_tokens,
            total_tokens: r.usage.total_tokens(),
            total_cost: r.usage.total_cost,
            request_count: r.usage.request_count,
        },
        tool_calls: r
            .tool_calls
            .into_iter()
            .map(tool_call_record_wire)
            .collect(),
        duration_ms: u64::try_from(r.duration.as_millis()).unwrap_or(u64::MAX),
        finish_reason: finish_reason_label(&r.finish_reason),
    }
}

/// Build [`ToolCallOutcomeWire`] from the kernel's tool-call outcome.
pub fn tool_call_outcome_wire(o: ToolCallOutcome) -> ToolCallOutcomeWire {
    match o {
        ToolCallOutcome::Success => ToolCallOutcomeWire {
            kind: "success".into(),
            output: None,
        },
        ToolCallOutcome::Error { output } => ToolCallOutcomeWire {
            kind: "error".into(),
            output: Some(output),
        },
        ToolCallOutcome::Denied => ToolCallOutcomeWire {
            kind: "denied".into(),
            output: None,
        },
        ToolCallOutcome::Blocked { reason } => ToolCallOutcomeWire {
            kind: "blocked".into(),
            output: Some(reason),
        },
    }
}

/// Build [`ToolCallRecordWire`] from the kernel's tool-call record.
pub fn tool_call_record_wire(r: rho_core::ToolCallRecord) -> ToolCallRecordWire {
    ToolCallRecordWire {
        name: r.name,
        arguments: r.arguments,
        outcome: tool_call_outcome_wire(r.outcome),
        duration_ms: r
            .duration
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    }
}

/// Map a [`LoopFinishReason`] to its JSON string label for `agent/end`.
fn finish_reason_label(reason: &LoopFinishReason) -> String {
    match reason {
        LoopFinishReason::Stop => "stop".into(),
        LoopFinishReason::MaxIterations => "max_iterations".into(),
        LoopFinishReason::Cancelled => "cancelled".into(),
        LoopFinishReason::RetryBudgetExhausted => "retry_budget_exhausted".into(),
        LoopFinishReason::ConsecutiveEmptyResponses => "consecutive_empty_responses".into(),
    }
}

/// Map a [`ToolRisk`] to its JSON string label.
pub fn risk_label(risk: ToolRisk) -> &'static str {
    match risk {
        ToolRisk::Read => "read",
        ToolRisk::Write => "write",
        ToolRisk::Destructive => "destructive",
        ToolRisk::Network => "network",
    }
}

/// Map an [`AgentState`] to its JSON string label.
pub fn state_name(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Thinking => "thinking",
        AgentState::AwaitingApproval => "awaiting_approval",
        AgentState::ExecutingTool => "executing_tool",
    }
}
