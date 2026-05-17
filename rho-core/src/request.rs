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
    /// Whether to enable streaming responses.
    ///
    /// When `false`, this field is omitted from the serialized JSON so that
    /// non-streaming requests remain compatible with all OpenAI-compatible
    /// servers.
    #[serde(skip_serializing_if = "is_false")]
    pub stream: bool,
    /// Maximum number of tokens the model may generate.
    ///
    /// When `Some`, sent as `max_tokens` in the request body. This is
    /// essential for reasoning/thinking models where the server's default
    /// output budget may be too small to accommodate both chain-of-thought
    /// reasoning and the actual content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
}

/// Helper for `#[serde(skip_serializing_if)]`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !value
}
