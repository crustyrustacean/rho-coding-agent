//! Streaming types for chat completions

use crate::response::FinishReason;
use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    pub delta: StreamDelta,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamDelta {
    pub content: Option<String>,
    // tool call fields will go here later
}
