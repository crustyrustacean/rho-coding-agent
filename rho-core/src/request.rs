//! [`ChatRequest`] — the body sent to the model API.

use crate::message::ChatMessage;
use crate::schema::ToolSchema;
use serde::Serialize;

/// A chat completion request body.
#[derive(Clone, Debug, Serialize)]
pub struct ChatRequest {
    /// The model identifier (e.g. `"qwen3-8b"`).
    pub model: String,
    /// The conversation history to send.
    pub messages: Vec<ChatMessage>,
    /// Tool definitions available to the model.
    pub tools: Vec<ToolSchema>,
}
