//! Approval gate implementations.
//!
//! Each mode (REPL, RPC, future TUI) provides its own [`ApprovalGate`]
//! implementation that decides *how* to ask the user for confirmation.

pub(crate) mod interactive;

pub(crate) use interactive::ReplApprovalGate;
