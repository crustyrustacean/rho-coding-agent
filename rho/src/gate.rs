//! Approval gate implementations.
//!
//! Each mode (REPL, RPC, future TUI) provides its own [`ApprovalGate`]
//! implementation that decides *how* to ask the user for confirmation.

pub(crate) mod repl;

pub(crate) use repl::ReplApprovalGate;
