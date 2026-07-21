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
    ///
    /// Some providers (e.g. LM Studio with certain models) omit this field.
    /// Defaults to zero counts when absent.
    #[serde(default)]
    pub usage: ModelUsage,
    /// Server-side statistics (reserved).
    #[serde(default)]
    pub stats: ModelStats,
    /// Server configuration fingerprint.
    #[serde(default, deserialize_with = "deserialize_null_string")]
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
///
/// Known variants map to the `OpenAI` spec. Unknown values from non-standard
/// providers are captured as [`FinishReason::Other`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    /// Standard stop — the response is complete.
    Stop,
    /// The model requested one or more tool invocations.
    ToolCalls,
    /// The model reached the token limit.
    Length,
    /// Generation was stopped by a content filter.
    ContentFilter,
    /// The model returned an unrecognised finish reason.
    Other(String),
}

/// Serialize as a plain string to match the wire format and the custom
/// [`Deserialize`] impl.
///
/// A derived serializer would emit `{"other": "…"}` for [`FinishReason::Other`]
/// (an externally-tagged newtype), which the deserializer — expecting a string —
/// cannot read back. That asymmetry made any session containing a non-standard
/// finish reason (e.g. `model_context_window_exceeded`) impossible to resume.
impl Serialize for FinishReason {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            Self::Stop => "stop",
            Self::ToolCalls => "tool_calls",
            Self::Length => "length",
            Self::ContentFilter => "content_filter",
            Self::Other(other) => other.as_str(),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt.as_deref() {
            Some("stop") => Ok(Self::Stop),
            Some("tool_calls") => Ok(Self::ToolCalls),
            Some("length") => Ok(Self::Length),
            Some("content_filter") => Ok(Self::ContentFilter),
            Some(other) => Ok(Self::Other(other.to_owned())),
            None => Ok(Self::Other("null".to_owned())),
        }
    }
}

/// Bridge rho-ai's streaming [`StopReason`](rho_ai::StopReason) to rho-core's
/// [`FinishReason`] for persistence.
///
/// `EndTurn` maps to [`FinishReason::Stop`] (a complete, natural stop).
impl From<rho_ai::StopReason> for FinishReason {
    fn from(reason: rho_ai::StopReason) -> Self {
        match reason {
            rho_ai::StopReason::EndTurn => Self::Stop,
            rho_ai::StopReason::ToolUse => Self::ToolCalls,
            rho_ai::StopReason::Length => Self::Length,
            rho_ai::StopReason::ContentFilter => Self::ContentFilter,
            rho_ai::StopReason::Other(s) => Self::Other(s),
        }
    }
}

/// The assistant message within a [`ModelChoice`].
#[derive(Clone, Debug, Deserialize)]
pub struct ModelMessage {
    /// Text content (may be empty when tool calls are present).
    #[serde(default, deserialize_with = "deserialize_null_string")]
    pub content: String,
    /// Chain-of-thought reasoning (model-specific; may be empty).
    #[serde(default, deserialize_with = "deserialize_null_string")]
    pub reasoning_content: String,
    /// Tool calls the model wants to invoke.
    #[serde(default)]
    pub tool_calls: Vec<ModelToolCall>,
}

/// Token usage statistics.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ModelUsage {
    /// Tokens in the prompt.
    #[serde(default)]
    pub prompt_tokens: usize,
    /// Tokens in the completion.
    #[serde(default)]
    pub completion_tokens: usize,
    /// Total tokens.
    #[serde(default)]
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

/// Deserialize a string field that may be `null` (e.g. from `OpenRouter`).
fn deserialize_null_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_reason_other_serializes_as_plain_string() {
        // Regression: `Other` used to serialize as `{"other": "…"}` (an
        // object), which the string-expecting deserializer couldn't read —
        // making any session with a non-standard finish reason unresumable.
        let reason = FinishReason::Other("model_context_window_exceeded".to_owned());
        let v = serde_json::to_value(&reason).unwrap();
        assert_eq!(v, "model_context_window_exceeded");

        let back: FinishReason = serde_json::from_value(v).unwrap();
        assert_eq!(back, reason);
    }

    #[test]
    fn finish_reason_known_variants_round_trip_as_strings() {
        for (reason, expected) in [
            (FinishReason::Stop, "stop"),
            (FinishReason::ToolCalls, "tool_calls"),
            (FinishReason::Length, "length"),
            (FinishReason::ContentFilter, "content_filter"),
        ] {
            let v = serde_json::to_value(&reason).unwrap();
            assert_eq!(v, expected);
            let back: FinishReason = serde_json::from_value(v).unwrap();
            assert_eq!(back, reason);
        }
    }
}
