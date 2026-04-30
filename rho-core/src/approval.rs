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
//!
//! # Config-driven behaviour
//!
//! [`ConfigApprovalPolicy`] checks per-tool overrides from [`RhoConfig`] before
//! falling back to the default risk-based policy. This allows config files to
//! set tools to `"auto"` (always allow), `"ask"` (require confirmation), or
//! `"deny"` (refuse entirely).

use crate::config::{ApprovalAction, ApprovalConfig, RhoConfig};
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

// ── ConfigApprovalPolicy ──────────────────────────────────────────────────────

/// Config-driven approval policy that checks per-tool overrides before
/// falling back to a risk-based default.
///
/// Per-tool actions (from [`ApprovalConfig`]):
/// - [`Auto`](ApprovalAction::Auto) — always allow, no confirmation needed
/// - [`Ask`](ApprovalAction::Ask) — require human confirmation
/// - [`Deny`](ApprovalAction::Deny) — refuse the tool entirely (treated as
///   requiring approval so the gate can issue a denial)
///
/// If a tool is not listed in the config, the fallback policy (defaulting to
/// risk-based) applies.
pub struct ConfigApprovalPolicy {
    /// Per-tool overrides from config.
    per_tool: ApprovalConfig,
    /// Fallback policy when a tool is not listed in config.
    fallback: Box<dyn ApprovalPolicy>,
}

impl ConfigApprovalPolicy {
    /// Create a config-driven policy from a [`RhoConfig`].
    ///
    /// Uses [`DefaultApprovalPolicy`] as the fallback for tools not listed
    /// in the config.
    pub fn new(config: &RhoConfig) -> Self {
        Self {
            per_tool: config.approval.clone(),
            fallback: Box::new(DefaultApprovalPolicy),
        }
    }

    /// Create a config-driven policy with a custom fallback.
    pub fn with_fallback(config: &RhoConfig, fallback: Box<dyn ApprovalPolicy>) -> Self {
        Self {
            per_tool: config.approval.clone(),
            fallback,
        }
    }

    /// Look up the configured action for a tool.
    ///
    /// Returns `None` if the tool is not listed in config.
    pub fn action_for(&self, tool_name: &ToolName) -> Option<ApprovalAction> {
        self.per_tool.per_tool.get(&**tool_name).copied()
    }

    /// Returns `true` if the tool is configured as `Deny`.
    pub fn is_denied(&self, tool_name: &ToolName) -> bool {
        self.action_for(tool_name) == Some(ApprovalAction::Deny)
    }
}

impl ApprovalPolicy for ConfigApprovalPolicy {
    fn requires_approval(&self, tool_name: &ToolName, risk: ToolRisk) -> bool {
        match self.action_for(tool_name) {
            Some(ApprovalAction::Auto) => false,
            Some(ApprovalAction::Ask | ApprovalAction::Deny) => true,
            None => self.fallback.requires_approval(tool_name, risk),
        }
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
