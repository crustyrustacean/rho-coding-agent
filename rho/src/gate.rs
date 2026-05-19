//! REPL-based approval gate for tool calls.
//!
//! Prints a tool-call preview to stderr and reads `y/N` from stdin.
//! This is the fallback gate used when the TUI is not available (Phase 4+).

use async_trait::async_trait;
use rho_core::{ModelToolCall, ToolRisk, approval::ApprovalGate};
use std::io::{self, BufRead, Write};

/// Prints a tool-call preview and reads `y/N` from stdin.
pub(crate) struct ReplApprovalGate;

#[async_trait]
impl ApprovalGate for ReplApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool {
        let risk_label = match risk {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Destructive => "destructive",
        };
        eprintln!();
        eprintln!("  Tool     : {}", call.function.name);
        eprintln!("  Risk     : {risk_label}");
        eprintln!("  Arguments: {}", call.function.arguments);
        eprint!("  Execute? [y/n] ");
        io::stderr().flush().ok();

        let mut line = String::new();
        let ok = io::stdin().lock().read_line(&mut line).is_ok();
        ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
