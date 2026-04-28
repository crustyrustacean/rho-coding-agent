//! Approval policy and gate.
//!
//! [`ApprovalPolicy`] decides *whether* a tool call needs human confirmation,
//! based on the tool's [`ToolRisk`]. [`ApprovalGate`] is the runtime mechanism
//! that actually asks the user.
//!
//! Separating them lets the binary (and future TUI) provide different gate
//! implementations while the policy stays consistent.
//!
//! # Default behaviour
//!
//! [`DefaultApprovalPolicy`] auto-approves [`ToolRisk::Read`] tools and requires
//! confirmation for [`ToolRisk::Write`] and [`ToolRisk::Destructive`] tools.

use crate::message::ModelToolCall;
use crate::newtypes::ToolName;
use crate::tool::ToolRisk;
use async_trait::async_trait;

// ── ApprovalPolicy ────────────────────────────────────────────────────────────

/// Decides whether a tool call requires human confirmation before execution.
///
/// The policy is a *static* classification based on tool name and risk level.
/// The [`ApprovalGate`] handles the runtime interaction with the user.
pub trait ApprovalPolicy: Send + Sync {
    /// Returns `true` if this tool call must be confirmed before it runs.
    fn requires_approval(&self, tool_name: &ToolName, risk: ToolRisk) -> bool;
}

/// Default policy: auto-approve reads; require confirmation for writes and
/// destructive operations.
pub struct DefaultApprovalPolicy;

impl ApprovalPolicy for DefaultApprovalPolicy {
    fn requires_approval(&self, _tool_name: &ToolName, risk: ToolRisk) -> bool {
        matches!(risk, ToolRisk::Write | ToolRisk::Destructive)
    }
}

/// Policy that approves every tool call without asking.
///
/// Use in tests and environments where the caller has already verified intent.
pub struct AutoApprovePolicy;

impl ApprovalPolicy for AutoApprovePolicy {
    fn requires_approval(&self, _tool_name: &ToolName, _risk: ToolRisk) -> bool {
        false
    }
}

// ── ApprovalGate ──────────────────────────────────────────────────────────────

/// Asks the user whether to proceed with a tool call.
///
/// The gate is responsible for rendering the prompt and collecting the response.
/// The bare REPL renders `Execute [tool]? [y/N]`; the Phase 4 TUI renders a
/// rich preview panel. Either way, the agent loop just calls this method and
/// acts on the `bool` result.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Request approval for `call`. Returns `true` if the user approves.
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool;
}
