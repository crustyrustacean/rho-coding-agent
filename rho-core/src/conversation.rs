//! [`AssistantResponse`] — the result of sending messages to the model.
//!
//! This module previously contained the `Conversation` struct, which was
//! superseded by [`Session`](crate::session::Session) in Phase 2.5. The
//! `AssistantResponse` enum remains as the return type of
//! [`Session::send_current`](crate::session::Session::send_current) and
//! [`run_loop`](crate::agent::run_loop).

use crate::message::ModelToolCall;

/// The result of sending messages to the model.
#[derive(Debug)]
pub enum AssistantResponse {
    /// The model completed with a text reply.
    Message(String),
    /// The model requested one or more tool invocations.
    ///
    /// The agent loop executes the tools and feeds results back into the
    /// session before re-sending.
    ToolCalls(Vec<ModelToolCall>),
}
