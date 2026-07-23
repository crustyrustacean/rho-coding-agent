//! Native `OpenAI` Responses API provider.
//!
//! This module converts rho's provider-neutral request types into the
//! `/v1/responses` wire format. Streaming response handling and HTTP routing
//! are layered on top of these adapters.

use crate::error::ProviderError;
use crate::service::{EventStream, LlmService};
use crate::sse::SseParser;
use crate::types::{
    LlmMessage, LlmRequest, ProviderConfig, StopReason, StreamEvent, StreamUsage, ToolCall,
    ToolDefinition,
};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::{debug, warn};

/// Request body sent to the `OpenAI` Responses endpoint.
#[derive(Debug, Serialize)]
struct ResponsesRequest {
    /// Model identifier.
    model: String,
    /// Stateless conversation input.
    input: Vec<InputItem>,
    /// System/developer instructions extracted from the conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    /// Function tools available to the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<FunctionTool>,
    /// Maximum number of output tokens, including reasoning tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<usize>,
    /// Reasoning configuration for reasoning-capable models.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    /// Enable SSE streaming.
    stream: bool,
    /// Disable provider-side response storage; rho owns conversation state.
    store: bool,
    /// Allow the model to request multiple function calls in one response.
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
}

/// A stateless Responses API input item.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputItem {
    /// User- or assistant-authored text.
    Message {
        /// Message role.
        role: MessageRole,
        /// Plain-text message content.
        content: String,
    },
    /// A function call made by the assistant in an earlier turn.
    FunctionCall {
        /// Stable call identifier used to correlate the function output.
        call_id: String,
        /// Function name.
        name: String,
        /// JSON-encoded function arguments.
        arguments: String,
    },
    /// The result of a function call.
    FunctionCallOutput {
        /// Identifier of the function call that produced this output.
        call_id: String,
        /// Plain-text function output.
        output: String,
    },
}

/// Role for a message input item.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    /// User-authored input.
    User,
    /// Model-authored output retained in stateless history.
    Assistant,
}

/// A custom function tool in the Responses wire format.
#[derive(Debug, Serialize)]
struct FunctionTool {
    /// Always `"function"`.
    r#type: &'static str,
    /// Function name.
    name: String,
    /// Human-readable function description.
    description: String,
    /// JSON Schema for function arguments.
    parameters: serde_json::Value,
    /// Whether `OpenAI` strict-schema validation is enabled.
    strict: bool,
}

/// Reasoning options accepted by reasoning-capable Responses models.
#[derive(Debug, Serialize)]
struct ReasoningConfig {
    /// Requested reasoning effort.
    effort: String,
    /// Request a streamed summary rather than hidden chain of thought.
    summary: &'static str,
}

/// Build the URL for the Responses endpoint.
///
/// Accepts an API base URL or a full Chat Completions/Responses URL. Known
/// endpoint suffixes are removed before `/responses` is appended, making the
/// operation idempotent and compatible with existing rho configurations.
fn responses_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let base = trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| trimmed.strip_suffix("/responses"))
        .unwrap_or(trimmed);
    format!("{base}/responses")
}

/// Convert unified tools into the flat Responses function-tool shape.
fn build_tools(tools: &[ToolDefinition]) -> Vec<FunctionTool> {
    tools
        .iter()
        .map(|tool| FunctionTool {
            r#type: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
            // rho's schemas predate OpenAI strict mode and may use constructs
            // that strict mode rejects. Keep existing permissive behavior.
            strict: false,
        })
        .collect()
}

/// Split system instructions from heterogeneous Responses input items.
fn build_input(messages: &[LlmMessage]) -> (Option<String>, Vec<InputItem>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();

    for message in messages {
        match message {
            LlmMessage::System(content) => instructions.push(content.clone()),
            LlmMessage::User(content) => input.push(InputItem::Message {
                role: MessageRole::User,
                content: content.clone(),
            }),
            LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    input.push(InputItem::Message {
                        role: MessageRole::Assistant,
                        content: content.clone(),
                    });
                }
                input.extend(tool_calls.iter().map(|call| InputItem::FunctionCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }));
            }
            LlmMessage::Tool {
                tool_call_id,
                content,
            } => input.push(InputItem::FunctionCallOutput {
                call_id: tool_call_id.clone(),
                output: content.clone(),
            }),
        }
    }

    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join("\n\n"))
    };
    (instructions, input)
}

/// Convert a provider-neutral request into a Responses request body.
fn build_request(request: &LlmRequest) -> Result<ResponsesRequest, ProviderError> {
    if request.model.is_empty() {
        return Err(ProviderError::Response {
            message: "model identifier is empty".to_owned(),
            raw: None,
        });
    }

    let (instructions, input) = build_input(&request.messages);
    let tools = build_tools(&request.tools);
    let reasoning = request
        .reasoning_effort
        .as_ref()
        .map(|effort| ReasoningConfig {
            effort: effort.clone(),
            summary: "auto",
        });

    Ok(ResponsesRequest {
        model: request.model.clone(),
        input,
        instructions,
        parallel_tool_calls: (!tools.is_empty()).then_some(true),
        tools,
        max_output_tokens: request.max_tokens,
        reasoning,
        stream: true,
        store: false,
    })
}

// ── Wire types (SSE response) ────────────────────────────────────────────────

/// A Responses API streaming event consumed by rho.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesEvent {
    /// Incremental assistant text.
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        /// Text fragment.
        delta: String,
    },
    /// A new output item, including a function-call start.
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        /// Position among all Responses output items.
        output_index: usize,
        /// Newly added output item.
        item: OutputItem,
    },
    /// Incremental function-call arguments.
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta {
        /// Position among all Responses output items.
        output_index: usize,
        /// Provider item identifier.
        item_id: String,
        /// JSON argument fragment.
        delta: String,
    },
    /// Final function-call arguments. The `name` field is optional — some
    /// models (e.g. gpt-5.6-sol) omit it since the name was established in
    /// the earlier `response.output_item.added` event.
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        /// Position among all Responses output items.
        output_index: usize,
        /// Provider item identifier.
        item_id: String,
        /// Function name (may be absent on some models).
        #[serde(default)]
        name: Option<String>,
        /// Complete JSON arguments.
        arguments: String,
    },
    /// A completed output item, used as a function-call fallback terminal.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        /// Position among all Responses output items.
        output_index: usize,
        /// Completed output item.
        item: OutputItem,
    },
    /// Incremental model-generated reasoning summary.
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta {
        /// Summary text fragment.
        delta: String,
    },
    /// Successful terminal response.
    #[serde(rename = "response.completed")]
    Completed {
        /// Terminal response metadata.
        response: ResponseEnvelope,
    },
    /// Terminal response stopped before normal completion.
    #[serde(rename = "response.incomplete")]
    Incomplete {
        /// Terminal response metadata.
        response: ResponseEnvelope,
    },
    /// Terminal response failure.
    #[serde(rename = "response.failed")]
    Failed {
        /// Failed response metadata.
        response: ResponseEnvelope,
    },
    /// Streaming API error.
    #[serde(rename = "error")]
    Error {
        /// Provider error code.
        #[serde(default)]
        code: Option<String>,
        /// Human-readable error message.
        message: String,
        /// Request parameter associated with the error.
        #[serde(default)]
        param: Option<String>,
    },
    /// Event types not needed by this implementation phase.
    #[serde(other)]
    Other,
}

/// A Responses output item relevant to rho.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum OutputItem {
    /// A custom function call.
    #[serde(rename = "function_call")]
    FunctionCall {
        /// Provider item identifier.
        #[serde(default)]
        id: Option<String>,
        /// Stable identifier used for the function output round trip.
        call_id: String,
        /// Function name.
        name: String,
        /// Arguments accumulated so far.
        #[serde(default)]
        arguments: String,
    },
    /// Any output item rho does not need to interpret.
    #[serde(other)]
    Other,
}

/// Fields consumed from a terminal response object.
#[derive(Debug, Default, Deserialize)]
struct ResponseEnvelope {
    /// Token usage, when reported.
    #[serde(default)]
    usage: Option<ResponseUsage>,
    /// Why an incomplete response stopped.
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
    /// Failure details for `response.failed`.
    #[serde(default)]
    error: Option<ResponseFailure>,
}

/// Token usage returned by the Responses API.
#[derive(Debug, Default, Deserialize)]
struct ResponseUsage {
    /// Number of input tokens.
    #[serde(default)]
    input_tokens: u64,
    /// Number of output tokens, including reasoning tokens.
    #[serde(default)]
    output_tokens: u64,
    /// Detailed input-token accounting.
    #[serde(default)]
    input_tokens_details: InputTokenDetails,
}

/// Detailed input-token usage.
#[derive(Debug, Default, Deserialize)]
struct InputTokenDetails {
    /// Number of cached input tokens.
    #[serde(default)]
    cached_tokens: u64,
}

/// Reason an incomplete response stopped.
#[derive(Debug, Default, Deserialize)]
struct IncompleteDetails {
    /// Provider reason string.
    #[serde(default)]
    reason: Option<String>,
}

/// Error embedded in a failed response.
#[derive(Debug, Deserialize)]
struct ResponseFailure {
    /// Provider error code.
    code: String,
    /// Human-readable error message.
    message: String,
}

impl ResponseUsage {
    /// Convert provider usage into rho's unified usage type.
    fn to_stream_usage(&self) -> StreamUsage {
        StreamUsage::new(self.input_tokens, self.output_tokens)
            .with_cached(self.input_tokens_details.cached_tokens)
    }
}

impl ResponseEnvelope {
    /// Convert optional provider usage, defaulting when it was omitted.
    fn stream_usage(&self) -> StreamUsage {
        self.usage
            .as_ref()
            .map_or_else(StreamUsage::default, ResponseUsage::to_stream_usage)
    }
}

/// Convert an incomplete-detail reason into rho's unified stop reason.
fn incomplete_stop_reason(details: Option<&IncompleteDetails>) -> StopReason {
    match details.and_then(|details| details.reason.as_deref()) {
        Some("max_output_tokens") => StopReason::Length,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::Other("incomplete".to_owned()),
    }
}

/// Format an API error while retaining its optional code and parameter.
fn format_api_error(code: Option<&str>, message: &str, param: Option<&str>) -> String {
    let mut formatted = code.map_or_else(String::new, |code| format!("{code}: "));
    formatted.push_str(message);
    if let Some(param) = param {
        formatted.push_str(" (parameter: ");
        formatted.push_str(param);
        formatted.push(')');
    }
    formatted
}

/// Construct a provider protocol error from inconsistent stream events.
fn protocol_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Sse {
        message: message.into(),
    }
}

/// Stateful correlation of item-indexed Responses function-call events.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    /// Calls keyed by the API's output index.
    calls: HashMap<usize, AccumulatedToolCall>,
    /// Next contiguous tool index exposed through rho's `StreamEvent` contract.
    next_tool_index: usize,
}

/// One function call under construction.
#[derive(Debug)]
struct AccumulatedToolCall {
    /// Contiguous index used by rho, independent of Responses output items.
    tool_index: usize,
    /// Provider output-item identifier.
    item_id: Option<String>,
    /// Stable call identifier sent back with function output.
    call_id: String,
    /// Function name.
    name: String,
    /// Accumulated JSON arguments.
    arguments: String,
    /// Whether `ToolUseStart` has been emitted.
    started: bool,
    /// Whether `ToolUseComplete` has been emitted.
    completed: bool,
}

impl ToolCallAccumulator {
    /// Register an output item and emit its start event once.
    fn add_item(
        &mut self,
        output_index: usize,
        item_id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        if let Some(existing) = self.calls.get(&output_index) {
            if existing.call_id != call_id || existing.name != name {
                return Err(protocol_error(format!(
                    "function-call output index {output_index} was reused"
                )));
            }
            return Ok(Vec::new());
        }

        let tool_index = self.next_tool_index;
        self.next_tool_index += 1;
        self.calls.insert(
            output_index,
            AccumulatedToolCall {
                tool_index,
                item_id,
                call_id: call_id.clone(),
                name: name.clone(),
                arguments,
                started: true,
                completed: false,
            },
        );
        Ok(vec![StreamEvent::ToolUseStart {
            index: tool_index,
            id: call_id,
            name,
        }])
    }

    /// Append one argument fragment to an existing function call.
    fn add_delta(
        &mut self,
        output_index: usize,
        item_id: &str,
        delta: String,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let call = self.calls.get_mut(&output_index).ok_or_else(|| {
            protocol_error(format!(
                "arguments delta arrived before function-call item {output_index}"
            ))
        })?;
        Self::validate_item_id(call, output_index, item_id)?;
        if call.completed {
            return Err(protocol_error(format!(
                "arguments delta arrived after function-call item {output_index} completed"
            )));
        }
        call.arguments.push_str(&delta);
        Ok(vec![StreamEvent::ToolUseInputDelta {
            index: call.tool_index,
            delta,
        }])
    }
    /// Complete a function call from the authoritative arguments-done event.
    ///
    /// When `name` is `Some`, it is validated against the name registered
    /// during `add_item`. When `None` (omitted by some models), the existing
    /// name is trusted.
    fn complete_arguments(
        &mut self,
        output_index: usize,
        item_id: &str,
        name: Option<&str>,
        arguments: String,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let call = self.calls.get_mut(&output_index).ok_or_else(|| {
            protocol_error(format!(
                "arguments completion arrived before function-call item {output_index}"
            ))
        })?;
        Self::validate_item_id(call, output_index, item_id)?;
        if let Some(name) = name
            && call.name != name
        {
            return Err(protocol_error(format!(
                "function name changed for output index {output_index}"
            )));
        }
        call.arguments = arguments;
        Self::complete(call)
    }

    /// Complete a function call from `output_item.done` when no argument-done
    /// event was delivered.
    fn complete_item(
        &mut self,
        output_index: usize,
        item_id: Option<&str>,
        call_id: &str,
        name: &str,
        arguments: &str,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut events = self.add_item(
            output_index,
            item_id.map(str::to_owned),
            call_id.to_owned(),
            name.to_owned(),
            arguments.to_owned(),
        )?;
        let call = self
            .calls
            .get_mut(&output_index)
            .expect("function-call item was inserted or already present");
        if let Some(item_id) = item_id {
            Self::validate_item_id(call, output_index, item_id)?;
        }
        if call.call_id != call_id || call.name != name {
            return Err(protocol_error(format!(
                "completed function-call item disagrees at output index {output_index}"
            )));
        }
        arguments.clone_into(&mut call.arguments);
        events.extend(Self::complete(call)?);
        Ok(events)
    }

    /// Determine the terminal stop reason, rejecting unfinished calls.
    fn terminal_reason(&self) -> Result<StopReason, ProviderError> {
        if let Some((output_index, _)) = self.calls.iter().find(|(_, call)| !call.completed) {
            return Err(protocol_error(format!(
                "response completed before function-call item {output_index}"
            )));
        }
        if self.calls.is_empty() {
            Ok(StopReason::EndTurn)
        } else {
            Ok(StopReason::ToolUse)
        }
    }

    /// Ensure events for an output index consistently name the same item.
    fn validate_item_id(
        call: &mut AccumulatedToolCall,
        output_index: usize,
        item_id: &str,
    ) -> Result<(), ProviderError> {
        if let Some(existing) = call.item_id.as_deref() {
            if existing != item_id {
                return Err(protocol_error(format!(
                    "item id changed for function-call output index {output_index}"
                )));
            }
        } else {
            call.item_id = Some(item_id.to_owned());
        }
        Ok(())
    }

    /// Emit one complete event, or nothing if already completed.
    fn complete(call: &mut AccumulatedToolCall) -> Result<Vec<StreamEvent>, ProviderError> {
        if call.completed {
            return Ok(Vec::new());
        }
        if !call.started {
            return Err(protocol_error("function call completed before it started"));
        }
        call.completed = true;
        Ok(vec![StreamEvent::ToolUseComplete {
            index: call.tool_index,
            tool_call: ToolCall {
                id: call.call_id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        }])
    }
}

// ── ResponsesService ─────────────────────────────────────────────────────────

/// An [`LlmService`] backed by `OpenAI`'s native Responses API.
#[derive(Clone, Debug)]
pub struct ResponsesService {
    /// Shared HTTP client.
    http: Client,
    /// Provider authentication and endpoint configuration.
    config: ProviderConfig,
}

impl ResponsesService {
    /// Create a Responses API service.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest` client builder cannot initialize its TLS or
    /// platform configuration.
    #[must_use]
    pub fn new(config: ProviderConfig) -> Self {
        let http = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_mins(2))
            .build()
            .expect("reqwest Client builder configuration is valid");
        Self { http, config }
    }

    /// Build and send one streaming Responses request.
    async fn send_streaming_request(
        &self,
        request: &LlmRequest,
    ) -> Result<EventStream, ProviderError> {
        let body = build_request(request)?;
        let url = responses_url(&self.config.base_url);
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.ok();
            return Err(ProviderError::HttpStatus {
                status: status_code,
                body,
                retryable: matches!(status_code, 429 | 500 | 502 | 503 | 504),
            });
        }

        Ok(Box::pin(ResponsesSseStream::new(Box::pin(
            response.bytes_stream(),
        ))))
    }
}

#[async_trait]
impl LlmService for ResponsesService {
    async fn chat_stream(&self, request: LlmRequest) -> Result<EventStream, ProviderError> {
        self.send_streaming_request(&request).await
    }
}

// ── SSE stream ────────────────────────────────────────────────────────────────

/// A stream adapter from Responses SSE bytes to unified rho events.
struct ResponsesSseStream {
    /// HTTP response body byte stream.
    byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
    /// Shared SSE framing parser.
    sse_parser: SseParser,
    /// Parsed events waiting to be yielded.
    pending: Vec<Result<StreamEvent, ProviderError>>,
    /// Item-indexed function-call correlation state.
    tool_acc: ToolCallAccumulator,
    /// Whether a terminal response or stream error has been observed.
    done: bool,
}

impl ResponsesSseStream {
    /// Wrap a streaming HTTP response body.
    fn new(
        byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
    ) -> Self {
        Self {
            byte_stream,
            sse_parser: SseParser::new(),
            pending: Vec::new(),
            tool_acc: ToolCallAccumulator::default(),
            done: false,
        }
    }

    /// Map one decoded Responses event into rho's stream contract.
    fn map_event(&mut self, event: ResponsesEvent) -> Result<Vec<StreamEvent>, ProviderError> {
        match event {
            ResponsesEvent::OutputTextDelta { delta } if !delta.is_empty() => {
                Ok(vec![StreamEvent::Text(delta)])
            }
            ResponsesEvent::ReasoningSummaryTextDelta { delta } if !delta.is_empty() => {
                Ok(vec![StreamEvent::Reasoning(delta)])
            }
            ResponsesEvent::OutputItemAdded {
                output_index,
                item:
                    OutputItem::FunctionCall {
                        id,
                        call_id,
                        name,
                        arguments,
                    },
            } => self
                .tool_acc
                .add_item(output_index, id, call_id, name, arguments),
            ResponsesEvent::FunctionCallArgumentsDelta {
                output_index,
                item_id,
                delta,
            } => self.tool_acc.add_delta(output_index, &item_id, delta),
            ResponsesEvent::FunctionCallArgumentsDone {
                output_index,
                item_id,
                name,
                arguments,
            } => {
                self.tool_acc
                    .complete_arguments(output_index, &item_id, name.as_deref(), arguments)
            }
            ResponsesEvent::OutputItemDone {
                output_index,
                item:
                    OutputItem::FunctionCall {
                        id,
                        call_id,
                        name,
                        arguments,
                    },
            } => self.tool_acc.complete_item(
                output_index,
                id.as_deref(),
                &call_id,
                &name,
                &arguments,
            ),
            ResponsesEvent::Completed { response } => self.map_completed(&response),
            ResponsesEvent::Incomplete { response } => self.map_incomplete(&response),
            ResponsesEvent::Failed { response } => {
                self.done = true;
                let message = response.error.map_or_else(
                    || "response failed without error details".to_owned(),
                    |error| format!("{}: {}", error.code, error.message),
                );
                Err(ProviderError::Response { message, raw: None })
            }
            ResponsesEvent::Error {
                code,
                message,
                param,
            } => {
                self.done = true;
                Err(ProviderError::Response {
                    message: format_api_error(code.as_deref(), &message, param.as_deref()),
                    raw: None,
                })
            }
            ResponsesEvent::OutputTextDelta { .. }
            | ResponsesEvent::ReasoningSummaryTextDelta { .. }
            | ResponsesEvent::OutputItemAdded { .. }
            | ResponsesEvent::OutputItemDone { .. }
            | ResponsesEvent::Other => Ok(Vec::new()),
        }
    }

    /// Map a successful terminal response.
    fn map_completed(
        &mut self,
        response: &ResponseEnvelope,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let reason = self.tool_acc.terminal_reason()?;
        self.done = true;
        Ok(vec![StreamEvent::Done {
            reason,
            usage: response.stream_usage(),
        }])
    }

    /// Map an incomplete terminal response, preferring complete tool calls.
    fn map_incomplete(
        &mut self,
        response: &ResponseEnvelope,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        let tool_reason = self.tool_acc.terminal_reason()?;
        self.done = true;
        let reason = if tool_reason == StopReason::ToolUse {
            StopReason::ToolUse
        } else {
            incomplete_stop_reason(response.incomplete_details.as_ref())
        };
        Ok(vec![StreamEvent::Done {
            reason,
            usage: response.stream_usage(),
        }])
    }

    /// Parse one arbitrary text chunk, which may contain partial or multiple
    /// SSE frames.
    fn parse_chunk(&mut self, text: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        let mut events = Vec::new();
        for sse_event in self.sse_parser.feed(text) {
            if self.done {
                break;
            }
            let data = sse_event.data.trim();
            // The Responses API terminates with a typed response event. Ignore
            // a compatibility sentinel if a proxy adds one.
            if data == "[DONE]" {
                continue;
            }

            let mapped = serde_json::from_str::<ResponsesEvent>(data)
                .map_err(|error| {
                    protocol_error(format!(
                        "failed to parse Responses event: {error}; payload: {}",
                        &data[..data.len().min(200)]
                    ))
                })
                .and_then(|event| self.map_event(event));
            match mapped {
                Ok(mapped_events) => events.extend(mapped_events.into_iter().map(Ok)),
                Err(error) => {
                    self.done = true;
                    events.push(Err(error));
                    break;
                }
            }
        }
        events
    }
}

impl Stream for ResponsesSseStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.pending.pop() {
            return Poll::Ready(Some(event));
        }
        if self.done {
            return Poll::Ready(None);
        }

        loop {
            match self.byte_stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    debug!(bytes = bytes.len(), "received Responses SSE bytes");
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let events = self.parse_chunk(&text);
                    if events.is_empty() {
                        if self.done {
                            return Poll::Ready(None);
                        }
                        continue;
                    }
                    events
                        .into_iter()
                        .rev()
                        .for_each(|event| self.pending.push(event));
                    return Poll::Ready(Some(
                        self.pending
                            .pop()
                            .expect("non-empty event batch was buffered"),
                    ));
                }
                Poll::Ready(Some(Err(error))) => {
                    self.done = true;
                    let message = format!("Responses SSE byte stream error: {error}");
                    warn!("{message}");
                    return Poll::Ready(Some(Err(ProviderError::Sse { message })));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(ProviderError::Sse {
                        message: "Responses stream ended before a terminal event".to_owned(),
                    })));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize a unified request through the Responses adapter.
    fn request_json(request: &LlmRequest) -> serde_json::Value {
        serde_json::to_value(build_request(request).expect("valid request"))
            .expect("request serializes")
    }

    #[test]
    fn responses_url_appends_to_base() {
        assert_eq!(
            responses_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn responses_url_normalizes_known_suffixes_and_slashes() {
        for endpoint in [
            "https://api.openai.com/v1/",
            "https://api.openai.com/v1/responses",
            "https://api.openai.com/v1/responses/",
            "https://api.openai.com/v1/chat/completions",
            "https://api.openai.com/v1/chat/completions/",
        ] {
            assert_eq!(
                responses_url(endpoint),
                "https://api.openai.com/v1/responses",
                "failed for {endpoint}"
            );
        }
    }

    #[test]
    fn system_messages_become_joined_instructions() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![
                LlmMessage::System("first".into()),
                LlmMessage::User("hello".into()),
                LlmMessage::System("second".into()),
            ],
        );
        let json = request_json(&request);
        assert_eq!(json["instructions"], "first\n\nsecond");
        assert_eq!(json["input"].as_array().unwrap().len(), 1);
        assert_eq!(json["input"][0]["role"], "user");
    }

    #[test]
    fn instructions_are_omitted_when_absent() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("instructions").is_none());
    }

    #[test]
    fn mixed_assistant_text_and_calls_preserve_order() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![LlmMessage::Assistant {
                content: Some("checking".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "call_a".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"a.rs"}"#.into(),
                    },
                    ToolCall {
                        id: "call_b".into(),
                        name: "list_dir".into(),
                        arguments: r#"{"path":"src"}"#.into(),
                    },
                ],
            }],
        );
        let json = request_json(&request);
        assert_eq!(json["input"][0]["type"], "message");
        assert_eq!(json["input"][0]["role"], "assistant");
        assert_eq!(json["input"][1]["type"], "function_call");
        assert_eq!(json["input"][1]["call_id"], "call_a");
        assert_eq!(json["input"][1]["name"], "read_file");
        assert_eq!(json["input"][2]["call_id"], "call_b");
    }

    #[test]
    fn tool_result_becomes_function_call_output() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![LlmMessage::Tool {
                tool_call_id: "call_exact".into(),
                content: "the result".into(),
            }],
        );
        let json = request_json(&request);
        assert_eq!(json["input"][0]["type"], "function_call_output");
        assert_eq!(json["input"][0]["call_id"], "call_exact");
        assert_eq!(json["input"][0]["output"], "the result");
    }

    #[test]
    fn function_tools_use_flat_permissive_shape() {
        let request =
            LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]).with_tools(vec![
                ToolDefinition::new(
                    "read_file",
                    "Read a file",
                    serde_json::json!({"type": "object"}),
                ),
            ]);
        let json = request_json(&request);
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "read_file");
        assert_eq!(json["tools"][0]["description"], "Read a file");
        assert_eq!(json["tools"][0]["parameters"]["type"], "object");
        assert_eq!(json["tools"][0]["strict"], false);
        assert!(json["tools"][0].get("function").is_none());
        assert_eq!(json["parallel_tool_calls"], true);
    }

    #[test]
    fn empty_tools_omit_tools_and_parallel_flag() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("tools").is_none());
        assert!(json.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn limits_reasoning_and_stateless_streaming_serialize() {
        let mut request =
            LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]).with_max_tokens(4096);
        request.reasoning_effort = Some("high".into());
        let json = request_json(&request);
        assert_eq!(json["max_output_tokens"], 4096);
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["reasoning"]["summary"], "auto");
        assert_eq!(json["stream"], true);
        assert_eq!(json["store"], false);
        assert!(json.get("max_tokens").is_none());
        assert!(json.get("reasoning_effort").is_none());
    }

    #[test]
    fn reasoning_is_omitted_when_not_configured() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("reasoning").is_none());
        assert!(json.get("max_output_tokens").is_none());
    }

    #[test]
    fn empty_model_is_rejected() {
        let request = LlmRequest::new("", vec![LlmMessage::User("hello".into())]);
        let error = build_request(&request).expect_err("empty model must fail");
        assert!(error.to_string().contains("model identifier is empty"));
    }

    /// Build a Responses stream from deterministic byte chunks.
    fn fixture_stream(chunks: Vec<&'static str>) -> ResponsesSseStream {
        let bytes = chunks
            .into_iter()
            .map(|chunk| Ok::<_, reqwest::Error>(bytes::Bytes::from_static(chunk.as_bytes())));
        ResponsesSseStream::new(Box::pin(futures::stream::iter(bytes)))
    }

    /// Start a one-shot local HTTP server and return its base URL plus a
    /// handle that resolves to the raw request.
    fn spawn_http_server(
        status: &'static str,
        response_body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture server");
        let address = listener.local_addr().expect("fixture server address");
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept fixture request");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set fixture read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let mut expected_length = None;
            loop {
                let read = socket.read(&mut buffer).expect("read fixture request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if expected_length.is_none()
                    && let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    expected_length = Some(header_end + 4 + content_length);
                }
                if expected_length.is_some_and(|length| request.len() >= length) {
                    break;
                }
            }

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            socket
                .write_all(response.as_bytes())
                .expect("write fixture response");
            String::from_utf8(request).expect("fixture request is UTF-8")
        });
        (format!("http://{address}/v1/chat/completions"), handle)
    }

    #[tokio::test]
    async fn text_and_usage_fixture_maps_to_unified_events() {
        use futures::StreamExt as _;

        let fixture = include_str!("../tests/fixtures/responses/text-and-usage.sse");
        let events = fixture_stream(vec![fixture])
            .collect::<Vec<Result<StreamEvent, ProviderError>>>()
            .await;
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], Ok(StreamEvent::Text(text)) if text == "Hello"));
        assert!(matches!(&events[1], Ok(StreamEvent::Text(text)) if text == " world"));
        match &events[2] {
            Ok(StreamEvent::Done { reason, usage }) => {
                assert_eq!(*reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 120);
                assert_eq!(usage.output_tokens, 17);
                assert_eq!(usage.cached_tokens, 40);
            }
            other => panic!("expected terminal usage event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn incomplete_reasons_map_to_stop_reasons() {
        use futures::StreamExt as _;

        for (provider_reason, expected) in [
            ("max_output_tokens", StopReason::Length),
            ("content_filter", StopReason::ContentFilter),
            (
                "provider_specific",
                StopReason::Other("provider_specific".into()),
            ),
        ] {
            let payload = format!(
                "data: {{\"type\":\"response.incomplete\",\"response\":{{\"incomplete_details\":{{\"reason\":\"{provider_reason}\"}}}}}}\n\n"
            );
            let leaked: &'static str = Box::leak(payload.into_boxed_str());
            let events = fixture_stream(vec![leaked]).collect::<Vec<_>>().await;
            assert!(matches!(
                &events[..],
                [Ok(StreamEvent::Done { reason, .. })] if *reason == expected
            ));
        }
    }

    #[tokio::test]
    async fn error_and_failed_events_surface_provider_errors() {
        use futures::StreamExt as _;

        let api_error = "data: {\"type\":\"error\",\"code\":\"bad_request\",\"message\":\"invalid value\",\"param\":\"input\"}\n\n";
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"generation failed\"}}}\n\n";
        for (fixture, expected) in [(api_error, "bad_request"), (failed, "server_error")] {
            let events = fixture_stream(vec![fixture]).collect::<Vec<_>>().await;
            assert_eq!(events.len(), 1);
            let error = events[0].as_ref().expect_err("fixture should fail");
            assert!(error.to_string().contains(expected));
        }
    }

    #[tokio::test]
    async fn malformed_event_is_an_error() {
        use futures::StreamExt as _;

        let events = fixture_stream(vec!["data: {not-json}\n\n"])
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Err(ProviderError::Sse { .. })));
    }

    #[tokio::test]
    async fn split_sse_chunks_reassemble_before_deserialization() {
        use futures::StreamExt as _;

        let events = fixture_stream(vec![
            "data: {\"type\":\"response.output_",
            "text.delta\",\"delta\":\"split\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        ])
        .collect::<Vec<_>>()
        .await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::Text(text)) if text == "split"));
        assert!(matches!(&events[1], Ok(StreamEvent::Done { .. })));
    }

    #[tokio::test]
    async fn eof_before_terminal_event_is_an_error() {
        use futures::StreamExt as _;

        let events = fixture_stream(vec![
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        ])
        .collect::<Vec<_>>()
        .await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::Text(_))));
        assert!(matches!(&events[1], Err(ProviderError::Sse { .. })));
    }

    #[tokio::test]
    async fn tool_call_fixture_accumulates_exact_call() {
        use futures::StreamExt as _;

        let fixture = include_str!("../tests/fixtures/responses/tool-call.sse");
        let events = fixture_stream(vec![fixture])
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("tool fixture parses");
        let complete_count = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolUseComplete { .. }))
            .count();
        assert_eq!(complete_count, 1);

        let accumulated = StreamEvent::accumulate(&events);
        assert_eq!(accumulated.stop_reason, StopReason::ToolUse);
        assert_eq!(accumulated.tool_calls.len(), 1);
        assert_eq!(accumulated.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            accumulated.tool_calls[0].function_name.as_deref(),
            Some("read_file")
        );
        assert_eq!(
            accumulated.tool_calls[0].arguments,
            r#"{"path":"src/lib.rs"}"#
        );
        assert_eq!(accumulated.usage.cached_tokens, 10);
    }

    #[tokio::test]
    async fn parallel_calls_use_contiguous_indexes_and_keep_interleaving() {
        use futures::StreamExt as _;

        let fixture = include_str!("../tests/fixtures/responses/parallel-tools-and-reasoning.sse");
        let events = fixture_stream(vec![fixture])
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("parallel fixture parses");
        let starts: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ToolUseStart { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![0, 1]);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ToolUseComplete { .. }))
                .count(),
            2
        );

        let accumulated = StreamEvent::accumulate(&events);
        assert_eq!(accumulated.reasoning, "I should inspect two paths.");
        assert_eq!(accumulated.stop_reason, StopReason::ToolUse);
        assert_eq!(accumulated.tool_calls.len(), 2);
        assert_eq!(accumulated.tool_calls[0].id.as_deref(), Some("call_a"));
        assert_eq!(accumulated.tool_calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert_eq!(accumulated.tool_calls[1].id.as_deref(), Some("call_b"));
        assert_eq!(accumulated.tool_calls[1].arguments, r#"{"path":"src"}"#);
    }

    #[tokio::test]
    async fn output_item_done_completes_call_as_fallback() {
        use futures::StreamExt as _;

        let fixture = concat!(
            "data: {\"type\":\"response.output_item.done\",\"output_index\":3,",
            "\"item\":{\"type\":\"function_call\",\"id\":\"fc_fallback\",",
            "\"call_id\":\"call_fallback\",\"name\":\"read_file\",",
            "\"arguments\":\"{\\\"path\\\":\\\"fallback.rs\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        );
        let events = fixture_stream(vec![fixture])
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("fallback fixture parses");
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolUseStart { index: 0, .. },
                StreamEvent::ToolUseComplete { index: 0, .. },
                StreamEvent::Done {
                    reason: StopReason::ToolUse,
                    ..
                }
            ]
        ));
    }

    #[tokio::test]
    async fn mismatched_item_id_is_a_protocol_error() {
        use futures::StreamExt as _;

        let fixture = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
            "\"item\":{\"type\":\"function_call\",\"id\":\"fc_right\",",
            "\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",",
            "\"output_index\":0,\"item_id\":\"fc_wrong\",\"delta\":\"{}\"}\n\n"
        );
        let events = fixture_stream(vec![fixture]).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::ToolUseStart { .. })));
        assert!(matches!(&events[1], Err(ProviderError::Sse { .. })));
    }

    #[tokio::test]
    async fn terminal_event_rejects_unfinished_function_call() {
        use futures::StreamExt as _;

        let fixture = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
            "\"item\":{\"type\":\"function_call\",\"id\":\"fc_open\",",
            "\"call_id\":\"call_open\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
        );
        let events = fixture_stream(vec![fixture]).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::ToolUseStart { .. })));
        assert!(matches!(&events[1], Err(ProviderError::Sse { .. })));
    }

    #[tokio::test]
    async fn service_posts_authenticated_request_and_streams_fixture() {
        use futures::StreamExt as _;

        let fixture = include_str!("../tests/fixtures/responses/text-and-usage.sse");
        let (endpoint, request_handle) = spawn_http_server("200 OK", fixture);
        let service = ResponsesService::new(ProviderConfig::new("secret-key", endpoint));
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let events = service
            .chat_stream(request)
            .await
            .expect("request succeeds")
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 3);

        let raw_request = request_handle.join().expect("fixture server joins");
        let (headers, body) = raw_request
            .split_once("\r\n\r\n")
            .expect("HTTP request has headers and body");
        assert!(headers.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer secret-key")
        );
        let json: serde_json::Value = serde_json::from_str(body).expect("request body is JSON");
        assert_eq!(json["model"], "gpt-5");
        assert_eq!(json["input"][0]["content"], "hello");
        assert_eq!(json["store"], false);
    }

    #[tokio::test]
    async fn service_classifies_retryable_http_status() {
        let (endpoint, request_handle) = spawn_http_server("429 Too Many Requests", "rate limited");
        let service = ResponsesService::new(ProviderConfig::new("key", endpoint));
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let result = service.chat_stream(request).await;
        assert!(matches!(
            result,
            Err(ProviderError::HttpStatus {
                status: 429,
                retryable: true,
                ..
            })
        ));
        request_handle.join().expect("fixture server joins");
    }

    #[tokio::test]
    async fn function_call_arguments_done_without_name_field() {
        use futures::StreamExt as _;

        // gpt-5.6-sol omits the `name` field from
        // `response.function_call_arguments.done` — the name was already
        // established in `response.output_item.added`. This must not fail.
        let fixture = "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"write_file\",\"arguments\":\"\"}}\n\n\
            data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0,\"item_id\":\"fc_1\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}\n\n\
            data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n";
        let events = fixture_stream(vec![fixture])
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("no-name done event must parse");
        assert!(matches!(
            &events[..],
            [
                StreamEvent::ToolUseStart { index: 0, .. },
                StreamEvent::ToolUseComplete { index: 0, .. },
                StreamEvent::Done {
                    reason: StopReason::ToolUse,
                    ..
                }
            ]
        ));
    }

    #[tokio::test]
    async fn function_call_arguments_done_with_wrong_name_still_errors() {
        use futures::StreamExt as _;

        // When `name` IS present and disagrees with the name from
        // `output_item.added`, it must still be a protocol error.
        let fixture = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,",
            "\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",",
            "\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",",
            "\"output_index\":0,\"item_id\":\"fc_1\",\"name\":\"write_file\",",
            "\"arguments\":\"{}\"}\n\n"
        );
        let events = fixture_stream(vec![fixture]).collect::<Vec<_>>().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Ok(StreamEvent::ToolUseStart { .. })));
        assert!(matches!(&events[1], Err(ProviderError::Sse { .. })));
    }
}
