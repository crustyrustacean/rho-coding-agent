//! OpenAI-compatible provider.
//!
//! Implements [`LlmService`] for any server that speaks the `OpenAI`
//! Chat Completions wire format (`OpenAI`, `DeepSeek`, `xAI`, `Groq`,
//! `OpenRouter`, `Ollama`, `LM Studio`, etc.).

use crate::error::ProviderError;
use crate::service::{EventStream, LlmService};
use crate::sse::SseParser;
use crate::types::{
    LlmMessage, ProviderConfig, StopReason, StreamEvent, StreamUsage, ToolCall, ToolDefinition,
};
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::{debug, warn};

// ── Wire types (request) ──────────────────────────────────────────────────────
//
// These types mirror the OpenAI wire format. They are only used for
// serde serialization/deserialization — field names are self-documenting
// in this context.

/// The request body sent to the `OpenAI` Chat Completions endpoint.
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    /// The model identifier.
    model: String,
    /// The conversation messages.
    messages: Vec<WireMessage>,
    /// Tool definitions (omitted when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    /// Whether to stream the response.
    stream: bool,
    /// Request usage stats in the final streaming chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// Reasoning effort for thinking-capable models.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    /// Maximum completion (output) tokens for this request.
    ///
    /// Sent explicitly so the provider uses rho's reserved completion budget
    /// rather than its own default. Several providers (e.g. `OpenRouter` routing
    /// to `Vertex`/`Bedrock`) default to the model's full `max_tokens` when this
    /// is absent, which causes `input + max_tokens > context_limit` rejections
    /// even when rho left ample headroom. Omitted from the wire when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
}

/// `stream_options` to request usage in the final streaming chunk.
#[derive(Debug, Serialize)]
struct StreamOptions {
    /// Whether to include usage in the final chunk.
    include_usage: bool,
}

/// A message in the `OpenAI` wire format.
#[derive(Debug, Serialize)]
#[serde(tag = "role")]
#[serde(rename_all = "lowercase")]
enum WireMessage {
    /// System instruction.
    System {
        /// The system prompt text.
        content: String,
    },
    /// User message.
    User {
        /// The user's input text.
        content: String,
    },
    /// Assistant message.
    Assistant {
        /// Text content (omitted when `None`).
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// Tool calls requested by the assistant (omitted when empty).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<WireToolCall>,
    },
    /// Tool execution result.
    Tool {
        /// The ID of the tool call this result corresponds to.
        tool_call_id: String,
        /// The tool's output text.
        content: String,
    },
}

/// A tool call in the `OpenAI` wire format.
#[derive(Debug, Serialize)]
struct WireToolCall {
    /// Unique identifier for this tool call.
    id: String,
    /// Always `"function"`.
    r#type: String,
    /// The function details.
    function: WireFunction,
}

/// The function portion of a tool call.
#[derive(Debug, Serialize)]
struct WireFunction {
    /// The function name.
    name: String,
    /// JSON-encoded arguments string.
    arguments: String,
}

/// A tool definition in the `OpenAI` wire format.
#[derive(Debug, Serialize)]
struct WireTool {
    /// Always `"function"`.
    r#type: String,
    /// The function details.
    function: WireToolFunction,
}

/// The function portion of a tool definition.
#[derive(Debug, Serialize)]
struct WireToolFunction {
    /// The function name.
    name: String,
    /// Human-readable description.
    description: String,
    /// JSON Schema for the parameters.
    parameters: serde_json::Value,
}

// ── Wire types (SSE response) ────────────────────────────────────────────────

/// A single SSE chunk from the streaming API.
#[derive(Debug, Deserialize)]
struct SseChunk {
    /// The completion choices (usually one).
    #[serde(default)]
    choices: Vec<SseChoice>,
    /// Usage statistics (present in the final chunk).
    #[serde(default)]
    usage: Option<SseUsage>,
}

/// A single choice within an SSE chunk.
#[derive(Debug, Deserialize)]
struct SseChoice {
    /// The incremental delta content.
    delta: SseDelta,
    /// The finish reason (present in the final chunk for this choice).
    finish_reason: Option<String>,
}

/// The delta content within an SSE choice.
#[derive(Debug, Default, Deserialize)]
struct SseDelta {
    /// The assistant role (present in the first chunk only).
    #[serde(default)]
    #[allow(dead_code)]
    role: Option<String>,
    /// Incremental text content.
    #[serde(default)]
    content: Option<String>,
    /// Incremental reasoning/thinking content (`DeepSeek`, `OpenAI` o-series).
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Vendor-specific reasoning field (GLM-5, some `OpenRouter` models).
    #[serde(default)]
    reasoning: Option<String>,
    /// Incremental tool call deltas.
    #[serde(default)]
    tool_calls: Option<Vec<SseToolCallDelta>>,
}

/// A tool call delta within an SSE choice.
#[derive(Debug, Deserialize)]
struct SseToolCallDelta {
    /// Index of this tool call in the batch.
    index: usize,
    /// Tool call ID (present in the first delta for this index).
    #[serde(default)]
    id: Option<String>,
    /// Function name and/or arguments delta.
    #[serde(default)]
    function: Option<SseFunctionDelta>,
}

/// A function delta within a tool call delta.
#[derive(Debug, Default, Deserialize)]
struct SseFunctionDelta {
    /// Function name (present in the first delta for this tool call).
    #[serde(default)]
    name: Option<String>,
    /// JSON arguments fragment.
    #[serde(default)]
    arguments: Option<String>,
}

/// Usage statistics (present in the final chunk when `stream_options.include_usage`).
#[derive(Debug, Deserialize, Default)]
struct SseUsage {
    /// Tokens in the prompt.
    #[serde(default)]
    prompt_tokens: u64,
    /// Tokens in the completion.
    #[serde(default)]
    completion_tokens: u64,
    /// Cost in USD (sent by `OpenRouter` and some providers).
    #[serde(default)]
    cost: Option<f64>,
    /// Cached prompt tokens (flat field used by `OpenRouter`).
    #[serde(default)]
    cached_tokens: Option<u64>,
    /// `OpenAI` nests cache stats under `prompt_tokens_details`.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

/// Nested cache breakdown used by `OpenAI`'s usage object.
#[derive(Debug, Deserialize, Default)]
struct PromptTokensDetails {
    /// Number of prompt tokens served from the cache.
    #[serde(default)]
    cached_tokens: Option<u64>,
}

impl SseUsage {
    /// Resolved cache-hit token count, preferring the flat field and falling
    /// back to `OpenAI`'s nested `prompt_tokens_details.cached_tokens`.
    fn resolved_cached(&self) -> u64 {
        self.cached_tokens
            .or_else(|| {
                self.prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens)
            })
            .unwrap_or(0)
    }
}

// ── Request builder ───────────────────────────────────────────────────────────

/// Convert unified messages into `OpenAI` wire-format messages.
fn build_messages(messages: Vec<LlmMessage>) -> Vec<WireMessage> {
    messages
        .into_iter()
        .map(|m| match m {
            LlmMessage::System(content) => WireMessage::System { content },
            LlmMessage::User(content) => WireMessage::User { content },
            LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                let wire_tool_calls: Vec<WireToolCall> = tool_calls
                    .into_iter()
                    .map(|tc| WireToolCall {
                        id: tc.id,
                        r#type: "function".to_string(),
                        function: WireFunction {
                            name: tc.name,
                            arguments: tc.arguments,
                        },
                    })
                    .collect();
                WireMessage::Assistant {
                    content,
                    tool_calls: wire_tool_calls,
                }
            }
            LlmMessage::Tool {
                tool_call_id,
                content,
            } => WireMessage::Tool {
                tool_call_id,
                content,
            },
        })
        .collect()
}

/// Convert unified tool definitions into `OpenAI` wire-format tools.
fn build_tools(tools: Vec<ToolDefinition>) -> Vec<WireTool> {
    tools
        .into_iter()
        .map(|t| WireTool {
            r#type: "function".to_string(),
            function: WireToolFunction {
                name: t.name,
                description: t.description,
                parameters: t.parameters,
            },
        })
        .collect()
}

/// Build the URL for the chat completions endpoint.
///
/// Accepts either a base URL (e.g. `https://api.openai.com/v1`) or a full
/// completions URL (e.g. `.../v1/chat/completions`).  In the latter case the
/// redundant suffix is stripped so callers don't accidentally double up.
fn completions_url(base_url: &str) -> String {
    let base = base_url
        .trim_end_matches('/')
        .trim_end_matches("/chat/completions");
    format!("{base}/chat/completions")
}

// ── Stream parser ─────────────────────────────────────────────────────────────

/// Accumulator for tool call arguments across SSE deltas.
///
/// `OpenAI` streams tool calls in chunks: the first delta carries `id` + `name`,
/// subsequent deltas carry `arguments` fragments. We accumulate these per-index
/// and emit `StreamEvent::ToolUseComplete` once all deltas for an index are
/// collected (signalled by `finish_reason`).
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    /// Accumulated tool calls indexed by position.
    calls: Vec<AccumulatedToolCall>,
}

/// A partially accumulated tool call.
#[derive(Debug, Default)]
struct AccumulatedToolCall {
    /// The tool call ID (set on the first delta).
    id: Option<String>,
    /// The function name (set on the first delta).
    name: Option<String>,
    /// Accumulated JSON arguments fragments.
    arguments: String,
}

impl ToolCallAccumulator {
    /// Process a tool call delta, returning `true` if we have data for this index.
    fn feed_delta(
        &mut self,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    ) {
        if self.calls.len() <= index {
            self.calls
                .resize_with(index + 1, AccumulatedToolCall::default);
        }
        let tc = &mut self.calls[index];
        if let Some(id) = id {
            tc.id = Some(id);
        }
        if let Some(name) = name {
            tc.name = Some(name);
        }
        if let Some(delta) = arguments_delta {
            tc.arguments.push_str(&delta);
        }
    }

    /// Drain all accumulated tool calls into `ToolUseComplete` events.
    fn drain(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let old = std::mem::take(&mut self.calls);
        for (index, tc) in old.into_iter().enumerate() {
            if let (Some(id), Some(name)) = (tc.id, tc.name) {
                events.push(StreamEvent::ToolUseComplete {
                    index,
                    tool_call: ToolCall {
                        id,
                        name,
                        arguments: tc.arguments,
                    },
                });
            }
        }
        events
    }
}

/// Convert a rho-ai `SseChunk` into [`StreamEvent`]s.
///
/// Returns a vector because one SSE chunk can produce multiple events
/// (e.g., text delta + done).
fn parse_sse_chunk(chunk: &SseChunk, tool_acc: &mut ToolCallAccumulator) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    let Some(choice) = chunk.choices.first() else {
        return events;
    };

    // Tool call deltas — emit ToolUseStart or ToolUseInputDelta.
    if let Some(tool_call_deltas) = &choice.delta.tool_calls {
        for tc_delta in tool_call_deltas {
            let func = tc_delta.function.as_ref();
            let has_id = tc_delta.id.is_some();
            let has_name = func.as_ref().is_some_and(|f| f.name.is_some());

            tool_acc.feed_delta(
                tc_delta.index,
                tc_delta.id.clone(),
                func.and_then(|f| f.name.clone()),
                func.and_then(|f| f.arguments.clone()),
            );

            if has_id || has_name {
                // First delta for this tool call — emit ToolUseStart.
                events.push(StreamEvent::ToolUseStart {
                    index: tc_delta.index,
                    id: tc_delta.id.clone().unwrap_or_default(),
                    name: func
                        .as_ref()
                        .and_then(|f| f.name.clone())
                        .unwrap_or_default(),
                });
            } else if func.as_ref().is_some_and(|f| f.arguments.is_some()) {
                // Subsequent delta — emit ToolUseInputDelta.
                events.push(StreamEvent::ToolUseInputDelta {
                    index: tc_delta.index,
                    delta: func
                        .as_ref()
                        .and_then(|f| f.arguments.clone())
                        .unwrap_or_default(),
                });
            }
        }
    }

    // Text delta.
    if let Some(text) = &choice.delta.content
        && !text.is_empty()
    {
        events.push(StreamEvent::Text(text.clone()));
    }

    // Reasoning delta (DeepSeek, OpenAI o-series use `reasoning_content`;
    // GLM-5 and some OpenRouter models use `reasoning`).
    let reasoning_text = choice
        .delta
        .reasoning_content
        .as_deref()
        .or(choice.delta.reasoning.as_deref());
    if let Some(reasoning) = reasoning_text
        && !reasoning.is_empty()
    {
        events.push(StreamEvent::Reasoning(reasoning.to_owned()));
    }

    // Finish reason — emit accumulated tool calls + Done.
    if let Some(reason_str) = &choice.finish_reason {
        // Flush accumulated tool calls before the Done event.
        let tool_events = tool_acc.drain();
        events.extend(tool_events);

        let stop_reason = match reason_str.as_str() {
            "stop" | "end_turn" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::Length,
            "content_filter" => StopReason::ContentFilter,
            other => StopReason::Other(other.to_string()),
        };

        let usage = chunk
            .usage
            .as_ref()
            .map(|u| {
                let cached = u.resolved_cached();
                let su = StreamUsage::new(u.prompt_tokens, u.completion_tokens)
                    .with_cached(cached);
                match u.cost {
                    Some(c) if c > 0.0 => su.with_cost(c),
                    _ => su,
                }
            })
            .unwrap_or_default();

        events.push(StreamEvent::Done {
            reason: stop_reason,
            usage,
        });
    }

    events
}

// ── OpenAiService ─────────────────────────────────────────────────────────────

/// An [`LlmService`] backed by an OpenAI-compatible API.
///
/// # Examples
///
/// ```ignore
/// use rho_ai::openai::OpenAiService;
/// use rho_ai::types::*;
/// use rho_ai::service::LlmService;
///
/// let service = OpenAiService::new(ProviderConfig::new(
///     env::var("OPENAI_API_KEY").unwrap(),
///     "https://api.openai.com/v1",
/// ));
///
/// let events = service.chat_stream(messages).await?;
/// ```
pub struct OpenAiService {
    /// The HTTP client.
    http: Client,
    /// Provider configuration (API key, base URL).
    config: ProviderConfig,
}

impl OpenAiService {
    /// Create a new OpenAI-compatible service.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest::Client` builder configuration is invalid.
    /// This only occurs for system-level errors (e.g. TLS backend failure).
    pub fn new(config: ProviderConfig) -> Self {
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_mins(2))
            .build()
            .expect("reqwest Client builder configuration is valid");
        Self { http, config }
    }

    /// Build and send the streaming HTTP request.
    async fn send_streaming_request(
        &self,
        request: &crate::types::LlmRequest,
    ) -> Result<EventStream, ProviderError> {
        let wire_messages = build_messages(request.messages.clone());
        let wire_tools = build_tools(request.tools.clone());

        let model = request.model.clone();
        if model.is_empty() {
            return Err(ProviderError::Response {
                message: "model identifier is empty".to_owned(),
                raw: None,
            });
        }

        let body = ChatCompletionRequest {
            model,
            messages: wire_messages,
            tools: wire_tools,
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: request.reasoning_effort.clone(),
            max_tokens: request.max_tokens,
        };

        let url = completions_url(&self.config.base_url);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body_text = response.text().await.ok();
            let retryable = matches!(status_code, 429 | 500 | 502 | 503 | 504);
            return Err(ProviderError::HttpStatus {
                status: status_code,
                body: body_text,
                retryable,
            });
        }

        let byte_stream = response.bytes_stream();
        let sse_stream = OpenAiSseStream::new(Box::pin(byte_stream));

        Ok(Box::pin(sse_stream))
    }
}

#[async_trait]
impl LlmService for OpenAiService {
    async fn chat_stream(
        &self,
        request: crate::types::LlmRequest,
    ) -> Result<EventStream, ProviderError> {
        self.send_streaming_request(&request).await
    }
}

// ── SSE Stream (futures::Stream impl) ────────────────────────────────────────

/// A `futures::Stream` that consumes a reqwest byte stream, parses SSE events,
/// and emits [`StreamEvent`] items.
///
/// This is similar to the `SseStream` in `rho-core` but produces the unified
/// `StreamEvent` type instead of `StreamChunk`.
struct OpenAiSseStream {
    /// Inner byte stream from reqwest.
    byte_stream: Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
    /// SSE line parser.
    sse_parser: SseParser,
    /// Tool call accumulator for streaming deltas.
    tool_acc: ToolCallAccumulator,
    /// Buffered events ready to be yielded.
    pending: Vec<StreamEvent>,
    /// Whether `[DONE]` has been received.
    done: bool,
}

impl OpenAiSseStream {
    /// Create a new stream wrapping the given byte source.
    fn new(
        byte_stream: Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
        >,
    ) -> Self {
        Self {
            byte_stream,
            sse_parser: SseParser::new(),
            tool_acc: ToolCallAccumulator::default(),
            pending: Vec::new(),
            done: false,
        }
    }

    /// Parse SSE events from a chunk of text and return [`StreamEvent`]s.
    fn parse_chunk(&mut self, text: &str) -> Vec<StreamEvent> {
        let sse_events = self.sse_parser.feed(text);
        let mut events = Vec::new();

        for sse_event in sse_events {
            let data = sse_event.data.trim();

            // Check for stream end sentinel.
            if data == "[DONE]" {
                // Flush any remaining tool calls.
                let tool_events = self.tool_acc.drain();
                events.extend(tool_events);
                self.done = true;
                continue;
            }

            // Parse the JSON payload.
            let sse_chunk = match serde_json::from_str::<SseChunk>(data) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "failed to parse SSE chunk: {e}; payload: {}",
                        &data[..data.len().min(200)]
                    );
                    continue;
                }
            };

            let chunk_events = parse_sse_chunk(&sse_chunk, &mut self.tool_acc);
            events.extend(chunk_events);
        }

        events
    }
}

impl Stream for OpenAiSseStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Return any buffered events first.
        if let Some(event) = self.pending.pop() {
            return Poll::Ready(Some(Ok(event)));
        }

        if self.done {
            return Poll::Ready(None);
        }

        // Poll the byte stream for more data.
        loop {
            match self.byte_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    debug!("received {} bytes from SSE stream", bytes.len());

                    let events = self.parse_chunk(&text);
                    if events.is_empty() {
                        if self.done {
                            return Poll::Ready(None);
                        }
                        // No events from this chunk, keep polling.
                        continue;
                    }

                    // Buffer all events; return the last one immediately,
                    // the rest will be returned on subsequent polls.
                    // We reverse so we can pop() from the end efficiently.
                    events.into_iter().rev().for_each(|e| self.pending.push(e));
                    return Poll::Ready(Some(Ok(self.pending.pop().unwrap())));
                }
                Poll::Ready(Some(Err(e))) => {
                    warn!("SSE byte stream error: {e}");
                    return Poll::Ready(None);
                }
                Poll::Ready(None) => {
                    // Inner stream exhausted.
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request builder tests ─────────────────────────────────────────────

    #[test]
    fn build_messages_system() {
        let msgs = vec![LlmMessage::System("You are helpful.".into())];
        let wire = build_messages(msgs);
        assert_eq!(wire.len(), 1);
        let json = serde_json::to_value(&wire[0]).unwrap();
        assert_eq!(json["role"], "system");
        assert_eq!(json["content"], "You are helpful.");
    }

    #[test]
    fn build_messages_user() {
        let msgs = vec![LlmMessage::User("Hello".into())];
        let wire = build_messages(msgs);
        let json = serde_json::to_value(&wire[0]).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn build_messages_assistant_text_only() {
        let msgs = vec![LlmMessage::Assistant {
            content: Some("Hi there".into()),
            tool_calls: vec![],
        }];
        let wire = build_messages(msgs);
        let json = serde_json::to_value(&wire[0]).unwrap();
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"], "Hi there");
        assert!(json.get("tool_calls").is_none()); // skip_serializing_if empty
    }

    // ── Empty model validation ──────────────────────────────────────────

    #[test]
    fn empty_model_returns_error() {
        use crate::LlmRequest;

        let service = OpenAiService::new(ProviderConfig::new("key", "http://localhost:1234/v1"));
        let request = LlmRequest {
            model: String::new(),
            messages: vec![LlmMessage::User("test".into())],
            tools: vec![],
            max_tokens: None,
            reasoning_effort: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(service.chat_stream(request));
        match result {
            Ok(_) => panic!("expected error for empty model"),
            Err(ref e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("model identifier is empty"),
                    "expected empty-model error, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn build_messages_assistant_with_tool_calls() {
        let msgs = vec![LlmMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_123".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"foo.rs"}"#.into(),
            }],
        }];
        let wire = build_messages(msgs);
        let json = serde_json::to_value(&wire[0]).unwrap();
        assert_eq!(json["role"], "assistant");
        assert!(json.get("content").is_none()); // skip_serializing_if None
        let tc = &json["tool_calls"][0];
        assert_eq!(tc["id"], "call_123");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "read_file");
        assert_eq!(tc["function"]["arguments"], r#"{"path":"foo.rs"}"#);
    }

    #[test]
    fn build_messages_tool_result() {
        let msgs = vec![LlmMessage::Tool {
            tool_call_id: "call_123".into(),
            content: "file contents".into(),
        }];
        let wire = build_messages(msgs);
        let json = serde_json::to_value(&wire[0]).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_123");
        assert_eq!(json["content"], "file contents");
    }

    #[test]
    fn build_tools_produces_correct_schema() {
        let tools = vec![ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        )];
        let wire = build_tools(tools);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "read_file");
        assert_eq!(json[0]["function"]["description"], "Read a file");
        assert_eq!(
            json[0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
    }

    #[test]
    fn completions_url_appends_path() {
        assert_eq!(
            completions_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_strips_trailing_slash() {
        assert_eq!(
            completions_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_idempotent_on_full_path() {
        assert_eq!(
            completions_url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_idempotent_on_full_path_with_trailing_slash() {
        assert_eq!(
            completions_url("https://api.openai.com/v1/chat/completions/"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    // ── Full request serialization ────────────────────────────────────────

    #[test]
    fn full_request_serializes_correctly() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![WireMessage::User {
                content: "Hello".into(),
            }],
            tools: vec![],
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            max_tokens: Some(2048),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
        assert_eq!(json["messages"][0]["role"], "user");
        assert!(json.get("tools").is_none()); // empty tools omitted
        assert_eq!(json["max_tokens"], 2048);
    }

    // ── SSE chunk parsing tests ───────────────────────────────────────────

    #[test]
    fn reasoning_effort_serializes_when_set() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: Some("medium".into()),
            max_tokens: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["reasoning_effort"], "medium");
    }

    #[test]
    fn reasoning_effort_omitted_when_none() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            max_tokens: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("max_tokens").is_none());
    }

    #[test]
    fn max_tokens_omitted_when_none() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            max_tokens: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("max_tokens").is_none());
    }

    #[test]
    fn max_tokens_sent_when_set() {
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![],
            tools: vec![],
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            reasoning_effort: None,
            max_tokens: Some(8192),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["max_tokens"], 8192);
    }

    #[test]
    fn parse_text_delta() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    role: None,
                    content: Some("Hello".into()),
                    reasoning_content: None,
                    reasoning: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Text(t) if t == "Hello"));
    }

    #[test]
    fn parse_empty_text_delta_skipped() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    role: None,
                    content: Some(String::new()),
                    reasoning_content: None,
                    reasoning: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_reasoning_delta() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    role: None,
                    content: None,
                    reasoning_content: Some("Let me think".into()),
                    reasoning: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Reasoning(t) if t == "Let me think"));
    }

    #[test]
    fn parse_finish_reason_stop() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("stop".into()),
            }],
            usage: Some(SseUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                ..Default::default()
            }),
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, usage } => {
                assert_eq!(*reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            _ => panic!("expected Done event"),
        }
    }

    #[test]
    fn parse_finish_reason_tool_calls() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, usage } => {
                assert_eq!(*reason, StopReason::ToolUse);
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.output_tokens, 0);
            }
            _ => panic!("expected Done event"),
        }
    }

    #[test]
    fn parse_finish_reason_unknown() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("some_custom_reason".into()),
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, .. } => {
                assert_eq!(*reason, StopReason::Other("some_custom_reason".into()));
            }
            _ => panic!("expected Done event"),
        }
    }

    #[test]
    fn parse_finish_reason_length() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("length".into()),
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, .. } => {
                assert_eq!(*reason, StopReason::Length);
            }
            _ => panic!("expected Done event"),
        }
    }

    #[test]
    fn parse_finish_reason_content_filter() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("content_filter".into()),
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done { reason, .. } => {
                assert_eq!(*reason, StopReason::ContentFilter);
            }
            _ => panic!("expected Done event"),
        }
    }

    // ── Tool call streaming ───────────────────────────────────────────────

    #[test]
    fn parse_tool_call_start_delta() {
        let chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![SseToolCallDelta {
                        index: 0,
                        id: Some("call_abc".into()),
                        function: Some(SseFunctionDelta {
                            name: Some("read_file".into()),
                            arguments: None,
                        }),
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUseStart { index, id, name } => {
                assert_eq!(*index, 0);
                assert_eq!(id, "call_abc");
                assert_eq!(name, "read_file");
            }
            _ => panic!("expected ToolUseStart"),
        }
    }

    #[test]
    fn parse_tool_call_arguments_delta() {
        // First: start
        let start_chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![SseToolCallDelta {
                        index: 0,
                        id: Some("call_abc".into()),
                        function: Some(SseFunctionDelta {
                            name: Some("read_file".into()),
                            arguments: None,
                        }),
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let _ = parse_sse_chunk(&start_chunk, &mut acc);

        // Second: arguments delta
        let args_chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![SseToolCallDelta {
                        index: 0,
                        id: None,
                        function: Some(SseFunctionDelta {
                            name: None,
                            arguments: Some(r#"{"path":"#.into()),
                        }),
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events = parse_sse_chunk(&args_chunk, &mut acc);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUseInputDelta { index, delta } => {
                assert_eq!(*index, 0);
                assert_eq!(delta, r#"{"path":"#);
            }
            _ => panic!("expected ToolUseInputDelta"),
        }

        // Third: more arguments
        let args_chunk2 = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![SseToolCallDelta {
                        index: 0,
                        id: None,
                        function: Some(SseFunctionDelta {
                            name: None,
                            arguments: Some(r#""foo.rs"}"#.into()),
                        }),
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events2 = parse_sse_chunk(&args_chunk2, &mut acc);
        assert_eq!(events2.len(), 1);
        match &events2[0] {
            StreamEvent::ToolUseInputDelta { index, delta } => {
                assert_eq!(*index, 0);
                assert_eq!(delta, r#""foo.rs"}"#);
            }
            _ => panic!("expected ToolUseInputDelta"),
        }

        // Fourth: finish
        let finish_chunk = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };
        let events3 = parse_sse_chunk(&finish_chunk, &mut acc);
        // Should have ToolUseComplete + Done
        assert_eq!(events3.len(), 2);
        match &events3[0] {
            StreamEvent::ToolUseComplete { index, tool_call } => {
                assert_eq!(*index, 0);
                assert_eq!(tool_call.id, "call_abc");
                assert_eq!(tool_call.name, "read_file");
                assert_eq!(tool_call.arguments, r#"{"path":"foo.rs"}"#);
            }
            _ => panic!("expected ToolUseComplete"),
        }
        match &events3[1] {
            StreamEvent::Done { reason, .. } => {
                assert_eq!(*reason, StopReason::ToolUse);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn parse_multiple_tool_calls() {
        let mut acc = ToolCallAccumulator::default();

        // Start both tool calls in one chunk
        let start = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![
                        SseToolCallDelta {
                            index: 0,
                            id: Some("c1".into()),
                            function: Some(SseFunctionDelta {
                                name: Some("read".into()),
                                arguments: None,
                            }),
                        },
                        SseToolCallDelta {
                            index: 1,
                            id: Some("c2".into()),
                            function: Some(SseFunctionDelta {
                                name: Some("edit".into()),
                                arguments: None,
                            }),
                        },
                    ]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events = parse_sse_chunk(&start, &mut acc);
        assert_eq!(events.len(), 2); // 2x ToolUseStart

        // Arguments for both
        let args = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta {
                    tool_calls: Some(vec![
                        SseToolCallDelta {
                            index: 0,
                            id: None,
                            function: Some(SseFunctionDelta {
                                name: None,
                                arguments: Some(r#"{"a":1}"#.into()),
                            }),
                        },
                        SseToolCallDelta {
                            index: 1,
                            id: None,
                            function: Some(SseFunctionDelta {
                                name: None,
                                arguments: Some(r#"{"b":2}"#.into()),
                            }),
                        },
                    ]),
                    ..Default::default()
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let events2 = parse_sse_chunk(&args, &mut acc);
        assert_eq!(events2.len(), 2); // 2x ToolUseInputDelta

        // Finish
        let finish = SseChunk {
            choices: vec![SseChoice {
                delta: SseDelta::default(),
                finish_reason: Some("tool_calls".into()),
            }],
            usage: None,
        };
        let events3 = parse_sse_chunk(&finish, &mut acc);
        assert_eq!(events3.len(), 3); // 2x ToolUseComplete + Done
    }

    // ── SSE stream integration tests ──────────────────────────────────────

    #[test]
    fn stream_parse_text_response() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
                       data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n\
                       data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);
        // Expect: Text("Hello"), Text(" world"), Done(EndTurn)
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], StreamEvent::Text(t) if t == "Hello"));
        assert!(matches!(&events[1], StreamEvent::Text(t) if t == " world"));
        assert!(matches!(
            &events[2],
            StreamEvent::Done {
                reason: StopReason::EndTurn,
                ..
            }
        ));
    }

    #[test]
    fn stream_parse_tool_call_response() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n\
                       data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":null,\"function\":{\"name\":null,\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n\
                       data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":null}\n\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);
        // Expect: ToolUseStart, ToolUseInputDelta, ToolUseComplete, Done(ToolUse)
        assert!(events.len() >= 4);
        assert!(matches!(
            &events[0],
            StreamEvent::ToolUseStart { index: 0, .. }
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::ToolUseInputDelta { index: 0, .. }
        ));
        assert!(matches!(
            &events[2],
            StreamEvent::ToolUseComplete { index: 0, .. }
        ));
        assert!(matches!(
            &events[3],
            StreamEvent::Done {
                reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn stream_usage_parsed_from_final_chunk() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}
\n\
                       data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50}}
\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);

        let done = events.iter().find_map(|e| match e {
            StreamEvent::Done { usage, .. } => Some(usage),
            _ => None,
        });
        assert!(done.is_some(), "expected Done event with usage");
        let usage = done.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    #[test]
    fn stream_usage_defaults_when_absent() {
        let sse_data =
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}
\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);

        let done = events.iter().find_map(|e| match e {
            StreamEvent::Done { usage, .. } => Some(usage),
            _ => None,
        });
        assert!(done.is_some(), "expected Done event");
        let usage = done.unwrap();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!(usage.cost.is_none());
    }

    #[test]
    fn stream_skips_malformed_json() {
        let sse_data = "data: {not json}\n\n\
                       data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Text(t) if t == "ok"));
    }

    #[test]
    fn stream_handles_split_chunks() {
        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));

        let e1 = stream.parse_chunk("data: {\"choices\":[{\"delta\":{\"con");
        assert!(e1.is_empty()); // incomplete JSON

        let e2 = stream.parse_chunk("tent\":\"Hi\"},\"finish_reason\":null}]}\n\n");
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], StreamEvent::Text(t) if t == "Hi"));
    }

    #[test]
    fn stream_done_without_finish_reason_flushes_tool_calls() {
        // Edge case: stream ends with [DONE] but no finish_reason chunk.
        let sse_data = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"test\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n\
                       data: [DONE]\n\n";

        let mut stream = OpenAiSseStream::new(Box::pin(futures::stream::empty()));
        let events = stream.parse_chunk(sse_data);
        // ToolUseStart + ToolUseComplete (flushed by [DONE])
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolUseStart { .. }));
        assert!(matches!(&events[1], StreamEvent::ToolUseComplete { .. }));
    }

    // ── No-choices chunk ──────────────────────────────────────────────────

    #[test]
    fn parse_chunk_with_no_choices() {
        let chunk = SseChunk {
            choices: vec![],
            usage: None,
        };
        let mut acc = ToolCallAccumulator::default();
        let events = parse_sse_chunk(&chunk, &mut acc);
        assert!(events.is_empty());
    }

    // ── ToolCallAccumulator ───────────────────────────────────────────────

    #[test]
    fn accumulator_drain_clears_state() {
        let mut acc = ToolCallAccumulator::default();
        acc.feed_delta(
            0,
            Some("id1".into()),
            Some("name1".into()),
            Some(r#"{"a":1}"#.into()),
        );
        let events = acc.drain();
        assert_eq!(events.len(), 1);
        // Second drain should be empty
        let events2 = acc.drain();
        assert!(events2.is_empty());
    }

    #[test]
    fn accumulator_handles_sparse_indices() {
        let mut acc = ToolCallAccumulator::default();
        acc.feed_delta(0, Some("id0".into()), Some("name0".into()), None);
        acc.feed_delta(2, Some("id2".into()), Some("name2".into()), None);
        let events = acc.drain();
        assert_eq!(events.len(), 2);
        // Index 0 and 2 present, index 1 was never started
        assert!(matches!(
            &events[0],
            StreamEvent::ToolUseComplete { index: 0, .. }
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::ToolUseComplete { index: 2, .. }
        ));
    }
}
