//! REPL-based approval gate for tool calls.
//!
//! Prints a tool-call preview to stdout and reads `y/N` from stdin.
//! This is the fallback gate used when the TUI is not available (Phase 4+).
//!
//! Writes to stdout (not stderr) to avoid terminals that render stderr
//! in a different colour.

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
        println!();
        println!("  Tool     : {}", call.function.name);
        println!("  Risk     : {risk_label}");
        println!("  Arguments: {}", call.function.arguments);
        print!("  Execute? [y/n] ");
        io::stdout().flush().ok();

        let mut line = String::new();
        let ok = io::stdin().lock().read_line(&mut line).is_ok();
        ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
