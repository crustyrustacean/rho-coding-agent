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
    /// The model reached the token limit.
    Length,
    /// Generation was stopped by a content filter.
    ContentFilter,
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
///
/// Note: the model identifier is **not** part of provider config.
/// The model is set per-request via [`LlmRequest::model`], sourced from
/// the session at the call site. This avoids stale model strings and
/// makes the data flow explicit.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// API key. Read from an environment variable by the factory.
    pub api_key: String,
    /// Base URL for the API endpoint.
    pub base_url: String,
}

impl ProviderConfig {
    /// Creates a new provider configuration.
    #[must_use]
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }
}

/// A chat completion request.
///
/// Bundles the model identifier, conversation messages, tool definitions,
/// and optional limits into a single struct. This is the unified input type
/// for [`LlmService::chat_stream`](crate::service::LlmService::chat_stream).
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// The model identifier (e.g. `"gpt-4o"`, `"claude-sonnet-4-20250514"`).
    pub model: String,
    /// The conversation history to send.
    pub messages: Vec<LlmMessage>,
    /// Tool definitions available to the model.
    pub tools: Vec<ToolDefinition>,
    /// Maximum number of tokens the model may generate.
    pub max_tokens: Option<usize>,
    /// Reasoning effort for thinking-capable models.
    ///
    /// Passed as `reasoning_effort` in the `OpenAI` Chat Completions request body.
    /// Common values: `"low"`, `"medium"`, `"high"`. Non-reasoning models
    /// silently ignore this field. `OpenRouter` proxies translate this to
    /// provider-specific thinking parameters.
    pub reasoning_effort: Option<String>,
}

impl LlmRequest {
    /// Creates a new request builder with the given model and messages.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<LlmMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            max_tokens: None,
            reasoning_effort: None,
        }
    }

    /// Add tool definitions to the request.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the maximum output tokens.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Whether tools are present in this request.
    #[must_use]
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }
}

/// The result of accumulating a stream of [`StreamEvent`]s.
///
/// Produced by [`StreamEvent::accumulate`]. Contains the assembled text,
/// reasoning, tool calls, and stop reason from a completed stream.
#[derive(Clone, Debug)]
pub struct AccumulatedResponse {
    /// Accumulated text content.
    pub text: String,
    /// Accumulated reasoning content.
    pub reasoning: String,
    /// Accumulated tool calls.
    pub tool_calls: Vec<AccumulatedToolCall>,
    /// Why the stream ended.
    pub stop_reason: StopReason,
    /// Token usage from the stream.
    pub usage: StreamUsage,
}

/// A tool call accumulated from streaming deltas.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccumulatedToolCall {
    /// The tool call ID.
    pub id: Option<String>,
    /// The function name.
    pub function_name: Option<String>,
    /// The accumulated arguments JSON.
    pub arguments: String,
}

impl StreamEvent {
    /// Accumulate a slice of events into an [`AccumulatedResponse`].
    ///
    /// Collects text, reasoning, and tool call deltas into complete strings
    /// and returns the finish reason. This is the unified equivalent of
    /// `StreamChunk::accumulate` — consumers should prefer this over
    /// working with raw events.
    pub fn accumulate(events: &[StreamEvent]) -> AccumulatedResponse {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
        let mut stop_reason = StopReason::EndTurn;
        let mut usage = StreamUsage::default();

        for event in events {
            match event {
                StreamEvent::Text(t) => text.push_str(t),
                StreamEvent::Reasoning(r) => reasoning.push_str(r),
                StreamEvent::ToolUseStart { index, id, name } => {
                    if tool_calls.len() <= *index {
                        tool_calls.resize_with(*index + 1, AccumulatedToolCall::default);
                    }
                    let tc = &mut tool_calls[*index];
                    tc.id = Some(id.clone());
                    tc.function_name = Some(name.clone());
                }
                StreamEvent::ToolUseInputDelta { index, delta } => {
                    if tool_calls.len() <= *index {
                        tool_calls.resize_with(*index + 1, AccumulatedToolCall::default);
                    }
                    tool_calls[*index].arguments.push_str(delta);
                }
                StreamEvent::ToolUseComplete { index, tool_call } => {
                    if tool_calls.len() <= *index {
                        tool_calls.resize_with(*index + 1, AccumulatedToolCall::default);
                    }
                    let tc = &mut tool_calls[*index];
                    tc.id = Some(tool_call.id.clone());
                    tc.function_name = Some(tool_call.name.clone());
                    tc.arguments.clone_from(&tool_call.arguments);
                }
                StreamEvent::Done { reason, usage: u } => {
                    stop_reason = reason.clone();
                    usage = u.clone();
                }
            }
        }

        AccumulatedResponse {
            text,
            reasoning,
            tool_calls,
            stop_reason,
            usage,
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
