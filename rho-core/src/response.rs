//! Types for model API responses.

use crate::message::ModelToolCall;
use serde::{Deserialize, Serialize};

/// A chat completion response from the model API.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelResponse {
    /// A unique identifier for this completion.
    pub id: String,
    /// The object type (e.g. `"chat.completion"`).
    pub object: String,
    /// Unix timestamp of creation.
    pub created: u64,
    /// The model used to generate the completion.
    pub model: String,
    /// The list of completion choices.
    pub choices: Vec<ModelChoice>,
    /// Token usage statistics.
    pub usage: ModelUsage,
    /// Server-side statistics (reserved).
    #[serde(default)]
    pub stats: ModelStats,
    /// Server configuration fingerprint.
    #[serde(default)]
    pub system_fingerprint: String,
}

/// Server-side statistics (currently empty).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ModelStats {}

/// A single completion choice.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelChoice {
    /// Index of this choice.
    pub index: usize,
    /// The assistant message for this choice.
    pub message: ModelMessage,
    /// Log probabilities (unused).
    pub logprobs: Option<serde_json::Value>,
    /// Why the model stopped generating.
    pub finish_reason: FinishReason,
}

/// Why the model stopped generating tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Standard stop — the response is complete.
    Stop,
    /// The model requested one or more tool invocations.
    ToolCalls,
    /// The model reached the token limit.
    Length,
    /// Generation was stopped by a content filter.
    ContentFilter,
}

/// The assistant message within a [`ModelChoice`].
#[derive(Clone, Debug, Deserialize)]
pub struct ModelMessage {
    /// Text content (may be empty when tool calls are present).
    #[serde(default)]
    pub content: String,
    /// Chain-of-thought reasoning (model-specific; may be empty).
    #[serde(default)]
    pub reasoning_content: String,
    /// Tool calls the model wants to invoke.
    #[serde(default)]
    pub tool_calls: Vec<ModelToolCall>,
}

/// Token usage statistics.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelUsage {
    /// Tokens in the prompt.
    pub prompt_tokens: usize,
    /// Tokens in the completion.
    pub completion_tokens: usize,
    /// Total tokens.
    pub total_tokens: usize,
    /// Breakdown of completion tokens.
    #[serde(default)]
    pub completion_tokens_details: Option<ReasoningTokens>,
}

/// Breakdown of completion tokens.
#[derive(Clone, Debug, Deserialize)]
pub struct ReasoningTokens {
    /// Tokens used for chain-of-thought reasoning.
    pub reasoning_tokens: usize,
}
