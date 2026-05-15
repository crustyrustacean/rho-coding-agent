//! Streaming types for chat completions

use crate::response::FinishReason;

/// An event in a streaming chat completion response
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of the message content.
    TextDelta(String),
    /// A delta containing tool call information
    ToolCallDelta,
    /// The end of the stream
    Done(FinishReason),
}