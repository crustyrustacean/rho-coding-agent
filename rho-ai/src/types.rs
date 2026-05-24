//! Unified types for provider-agnostic LLM communication.
//!
//! These types are the only data structures that cross the boundary between
//! `rho-ai` and its consumers. Provider-specific wire formats are confined
//! to each provider module and never leak outside this crate.

use serde::{Deserialize, Serialize};

/// A chat message in the unified format.
///
/// Each provider module converts between its wire format and this type.
/// Downstream code (the agent loop, session, context assembly) works
/// exclusively with `LlmMessage`.
#[derive(Debug, Clone)]
pub enum LlmMessage {
    /// System prompt. Providers handle placement differently:
    /// - `OpenAI`: first message with `role: "system"`
    /// - `Anthropic`: top-level `system` field
    /// - `Google`: top-level `systemInstruction`
    System(String),

    /// User message.
    User(String),

    /// Assistant message, optionally containing tool calls.
    Assistant {
        /// Text content (may be empty if the response was only tool calls).
        content: Option<String>,
        /// Tool calls made by the assistant (may be empty for pure text).
        tool_calls: Vec<ToolCall>,
    },

    /// Tool execution result.
    Tool {
        /// ID matching the `ToolCall.id` that produced this result.
        tool_call_id: String,
        /// The tool's output content.
        content: String,
    },
}

/// A tool call from the LLM.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Unique identifier for this call (used to correlate with `ToolResult`).
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The tool arguments as a raw JSON string.
    /// Parsed lazily by the consumer — keeps the boundary clean.
    pub arguments: String,
}

/// A tool definition to present to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// The tool name (matches the function/command the LLM will invoke).
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Creates a new tool definition.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// An event from the LLM response stream.
///
/// Providers parse their SSE formats into these variants. The agent loop
/// consumes the stream and reacts to each event type.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A text content token.
    Text(String),

    /// A reasoning/thinking token (e.g., `DeepSeek`'s thinking, `Anthropic`'s extended thinking).
    Reasoning(String),

    /// A tool call is starting.
    ToolUseStart {
        /// Index of this tool call in the batch (0-based).
        index: usize,
        /// Unique identifier for the call.
        id: String,
        /// The tool name.
        name: String,
    },

    /// A partial chunk of tool call arguments (streaming JSON delta).
    ToolUseInputDelta {
        /// Index of the tool call this delta belongs to.
        index: usize,
        /// Partial JSON string fragment.
        delta: String,
    },

    /// A tool call is complete with its full arguments.
    ToolUseComplete {
        /// Index of the tool call.
        index: usize,
        /// The fully assembled tool call.
        tool_call: ToolCall,
    },

    /// The stream has ended.
    Done {
        /// Why the stream stopped.
        reason: StopReason,
        /// Token usage statistics.
        usage: StreamUsage,
    },
}

/// Why the LLM stopped generating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished its turn naturally.
    EndTurn,
    /// The model wants to call tools.
    ToolUse,
    /// Any other reason (provider-specific).
    Other(String),
}

/// Token usage and optional cost from a completed stream.
#[derive(Debug, Clone, Default)]
pub struct StreamUsage {
    /// Number of tokens in the input (prompt + conversation history).
    pub input_tokens: u64,
    /// Number of tokens in the output (completion).
    pub output_tokens: u64,
    /// Cost in USD, if the provider reports it.
    pub cost: Option<f64>,
}

impl StreamUsage {
    /// Creates a new usage record.
    #[must_use]
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cost: None,
        }
    }

    /// Creates a usage record with cost information.
    #[must_use]
    pub fn with_cost(mut self, cost: f64) -> Self {
        self.cost = Some(cost);
        self
    }
}

/// Configuration for creating an LLM service instance.
///
/// This is the minimal set of parameters every provider needs.
/// Provider-specific config (extra headers, custom paths) is handled
/// by each provider's own config type.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// The model identifier (e.g., `"gpt-4o"`, `"claude-sonnet-4-20250514"`).
    pub model: String,
    /// API key. Read from an environment variable by the factory.
    pub api_key: String,
    /// Base URL for the API endpoint.
    pub base_url: String,
}

impl ProviderConfig {
    /// Creates a new provider configuration.
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }
}

/// Which provider backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// `OpenAI` and any compatible server (`DeepSeek`, `xAI`, `Groq`, `OpenRouter`, `Ollama`, `LM Studio`, etc.).
    OpenAi,
    /// `Anthropic` (`Claude` models).
    Anthropic,
    /// `Google Gemini`.
    Google,
}
