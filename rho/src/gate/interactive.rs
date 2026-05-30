//! REPL-based approval gate for tool calls.
//!
//! Delegates formatting to [`crate::presenter::ReplPresenter`] and reads
//! `y/N` from stdin.

use async_trait::async_trait;
use rho_core::{ModelToolCall, ToolRisk, approval::ApprovalGate};
use std::io::{self, BufRead};

/// Prints a tool-call preview and reads `y/N` from stdin.
pub(crate) struct ReplApprovalGate;

#[async_trait]
impl ApprovalGate for ReplApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool {
        let risk_label = match risk {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Destructive => "destructive",
            ToolRisk::Network => "network",
        };
        crate::presenter::ReplPresenter::approval_prompt(
            &call.function.name,
            risk_label,
            &call.function.arguments,
        );

        let mut line = String::new();
        let ok = io::stdin().lock().read_line(&mut line).is_ok();
        ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
