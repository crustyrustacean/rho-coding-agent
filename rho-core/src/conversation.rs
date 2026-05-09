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
    ///
    /// `reasoning_content` carries chain-of-thought reasoning from reasoning
    /// models (DeepSeek-R1, Qwen3, etc.). Empty for non-reasoning models.
    Message {
        /// The model's final text output.
        text: String,
        /// Chain-of-thought reasoning content, if the model emitted any.
        reasoning_content: String,
    },
    /// The model requested one or more tool invocations.
    ///
    /// The agent loop executes the tools and feeds results back into the
    /// session before re-sending.
    ToolCalls(Vec<ModelToolCall>),
    /// The model hit the token limit before completing its response.
    ///
    /// This occurs when `finish_reason` is `Length` — the model ran out of
    /// completion tokens. With reasoning models, this often means the model
    /// spent all its tokens on chain-of-thought (`reasoning_content`) and
    /// produced nothing in `content`.
    ///
    /// The agent loop handles this by attempting compaction to free context
    /// space, then retrying. If compaction is not possible, a user-facing
    /// explanation is returned.
    LengthTruncated {
        /// Any text content the model produced before being truncated.
        /// May be empty (e.g. when a reasoning model spent all tokens on
        /// chain-of-thought).
        content: String,
        /// Chain-of-thought reasoning content from the model, if available.
        /// Non-empty for reasoning models (DeepSeek-R1, Qwen3, etc.) that
        /// ran out of tokens during thinking.
        reasoning_content: String,
    },
}
