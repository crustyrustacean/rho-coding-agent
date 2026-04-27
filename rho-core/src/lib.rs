//! Core library for rho-coding-agent.
//!
//! Provides domain types for chat completion requests/responses, an HTTP client for
//! communicating with OpenAI-compatible model APIs, and a [`Conversation`] type that
//! manages message history.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Domain types: request ──────────────────────────────────────────────────────

/// A chat completion request sent to the model API.
#[derive(Clone, Debug, Serialize)]
pub struct ChatRequest {
    /// The model identifier (e.g. `"qwen3-8b"`).
    pub model: String,
    /// The conversation messages to send.
    pub messages: Vec<ChatMessage>,
    /// The tools available for the model to call.
    pub tools: Vec<Tool>,
}

/// A single message in a conversation.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    /// The role of the message author.
    pub role: Role,
    /// The text content of the message.
    pub content: String,
}

/// A tool that the model may invoke.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tool {
    /// The tool type (e.g. `"function"`).
    #[serde(rename = "type")]
    pub tool_type: String,
    /// The function definition describing the tool.
    pub function: ToolFunction,
}

/// The function signature of a tool.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolFunction {
    /// The name of the function.
    pub name: String,
    /// A description of what the function does.
    pub description: String,
    /// The JSON Schema parameters the function accepts.
    pub parameters: ToolParameters,
}

/// The JSON Schema describing a tool's parameters.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolParameters {
    /// The schema type (typically `"object"`).
    #[serde(rename = "type")]
    pub parameters_type: String,
    /// The individual parameter properties.
    pub properties: std::collections::HashMap<String, ToolParameterProperty>,
    /// The names of parameters that must always be provided.
    pub required: Vec<String>,
}

/// A single parameter property within a [`ToolParameters`] schema.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolParameterProperty {
    /// The JSON type of the parameter (e.g. `"string"`, `"number"`).
    #[serde(rename = "type")]
    pub property_type: String,
    /// A human-readable description of the parameter.
    pub description: String,
}

/// The role of a message author in a conversation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A system-level instruction that guides model behaviour.
    System,
    /// A message from the end user.
    User,
    /// A response from the assistant.
    Assistant,
    /// The result of a tool invocation, fed back to the model.
    Tool,
}

// ── Domain types: response ─────────────────────────────────────────────────────

/// A chat completion response from the model API.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelResponse {
    /// A unique identifier for the completion.
    pub id: String,
    /// The object type (e.g. `"chat.completion"`).
    pub object: String,
    /// Unix timestamp of when the completion was created.
    pub created: usize,
    /// The model used to generate the completion.
    pub model: String,
    /// The list of completion choices.
    pub choices: Vec<ModelChoice>,
    /// Token usage statistics.
    pub usage: ModelUsage,
    /// Server-side statistics (currently empty).
    pub stats: ModelStats,
    /// A fingerprint identifying the server configuration.
    pub system_fingerprint: String,
}

/// Server-side statistics (reserved, currently empty).
#[derive(Clone, Debug, Deserialize)]
pub struct ModelStats {}

/// A single completion choice within a [`ModelResponse`].
#[derive(Clone, Debug, Deserialize)]
pub struct ModelChoice {
    /// The index of this choice in the list.
    pub index: usize,
    /// The assistant message for this choice.
    pub message: ModelMessage,
    /// Log probabilities, if requested.
    pub logprobs: Option<String>,
    /// The reason the model stopped generating.
    pub finish_reason: FinishReason,
}

/// The reason the model stopped generating tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model issued a standard stop token.
    Stop,
    /// The model requested one or more tool invocations.
    ToolCalls,
    /// The model reached the maximum token limit.
    Length,
    /// Generation was stopped by a content filter.
    ContentFilter,
}

/// The assistant message within a [`ModelChoice`].
#[derive(Clone, Debug, Deserialize)]
pub struct ModelMessage {
    /// The role (always [`Role::Assistant`]).
    pub role: Role,
    /// The text content of the response.
    pub content: String,
    /// Chain-of-thought reasoning content (may be empty).
    pub reasoning_content: String,
    /// Any tool calls the model wants to invoke.
    pub tool_calls: Vec<ModelToolCall>,
}

/// A tool call requested by the model.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelToolCall {
    /// The call type (e.g. `"function"`).
    #[serde(rename = "type")]
    pub tool_call_type: String,
    /// A unique identifier for this tool call.
    pub id: String,
    /// The function invocation details.
    #[serde(rename = "function")]
    pub tool_call_function: ToolCallFunction,
}

/// The function name and arguments of a [`ModelToolCall`].
#[derive(Clone, Debug, Deserialize)]
pub struct ToolCallFunction {
    /// The name of the function to invoke.
    pub name: String,
    /// The JSON-encoded arguments for the function.
    pub arguments: String,
}

/// Token usage statistics returned with a [`ModelResponse`].
#[derive(Clone, Debug, Deserialize)]
pub struct ModelUsage {
    /// The number of tokens in the prompt.
    pub prompt_tokens: usize,
    /// The number of tokens in the completion.
    pub completion_tokens: usize,
    /// The total number of tokens (prompt + completion).
    pub total_tokens: usize,
    /// Breakdown of completion tokens.
    pub completion_tokens_details: ReasoningTokens,
}

/// Breakdown of completion tokens, including reasoning overhead.
#[derive(Clone, Debug, Deserialize)]
pub struct ReasoningTokens {
    /// The number of tokens used for chain-of-thought reasoning.
    pub reasoning_tokens: usize,
}

/// The result of sending a message to the model.
///
/// When the model responds with text, this is [`AssistantResponse::Message`].
/// When the model requests a tool invocation, this is [`AssistantResponse::ToolCall`].
pub enum AssistantResponse {
    /// The model returned a text reply.
    Message(String),
    /// The model requested a tool invocation.
    ToolCall {
        /// The name of the tool to invoke.
        name: String,
        /// The JSON-encoded arguments for the tool.
        arguments: String,
    },
}

/// Errors that can occur when interacting with the model API.
#[derive(Debug, Error)]
pub enum RhoError {
    /// An HTTP request failed (network error, non-2xx status, etc.).
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A response body could not be parsed as JSON.
    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    /// An unexpected error occurred.
    #[error(transparent)]
    Unexpected(#[from] anyhow::Error),
}

/// A specialized `Result` type for rho-coding-agent operations.
pub type Result<T> = std::result::Result<T, RhoError>;

/// An HTTP client for communicating with an OpenAI-compatible chat completions API.
pub struct RhoHttpClient {
    /// The underlying `reqwest` HTTP client.
    http_client: Client,
    /// The base URL of the model API endpoint.
    base_url: String,
}

impl RhoHttpClient {
    /// Create a new client pointing at the default local API endpoint
    /// (`http://localhost:1234/v1/chat/completions`).
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            base_url: "http://localhost:1234/v1/chat/completions".to_string(),
        }
    }

    /// Send a chat completion request to the model API.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the response body cannot be parsed as a
    /// [`ModelResponse`].
    pub async fn chat(&self, chat_request: &ChatRequest) -> Result<ModelResponse> {
        Ok(self
            .http_client
            .post(&self.base_url)
            .json(chat_request)
            .send()
            .await?
            .json::<ModelResponse>()
            .await?)
    }
}

impl Default for RhoHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A conversation with a model, maintaining message history across turns.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Conversation {
    /// The model identifier used for chat completion requests.
    model: String,
    /// The accumulated conversation history.
    messages: Vec<ChatMessage>,
    /// The tools available for the model to call.
    tools: Vec<Tool>,
}

impl Conversation {
    /// Create a new conversation.
    ///
    /// If `system_prompt` is `Some`, it is prepended as a [`Role::System`] message.
    /// The `tools` list is sent with every request so the model can decide whether
    /// to invoke them.
    pub fn new(model: String, system_prompt: Option<&str>, tools: Vec<Tool>) -> Self {
        Self {
            model,
            tools,
            messages: match system_prompt {
                Some(prompt) => vec![ChatMessage {
                    role: Role::System,
                    content: prompt.to_string(),
                }],
                None => vec![],
            },
        }
    }

    /// Send a user message, append it to the history, and return the model's reply.
    ///
    /// If the model responds with text, the assistant message is appended to the
    /// history and [`AssistantResponse::Message`] is returned.
    ///
    /// If the model requests a tool call, the assistant message is **not** appended
    /// (the caller is responsible for executing the tool and feeding the result back).
    ///
    /// # Errors
    ///
    /// Returns a [`RhoError`] if the HTTP request fails or the response body
    /// cannot be parsed as a [`ModelResponse`].
    pub async fn send(
        &mut self,
        message: &str,
        http_client: &RhoHttpClient,
    ) -> Result<AssistantResponse> {
        let user_message = ChatMessage {
            role: Role::User,
            content: message.to_string(),
        };

        self.messages.push(user_message);

        let user_chat_request = ChatRequest {
            model: self.model.clone(),
            messages: self.messages.clone(),
            tools: self.tools.clone(),
        };

        let assistant_chat_response = http_client.chat(&user_chat_request).await?;

        let content = assistant_chat_response.choices[0].message.content.clone();

        let finish_reason = assistant_chat_response.choices[0].finish_reason.clone();

        let assistant_response = if let FinishReason::ToolCalls = finish_reason {
            AssistantResponse::ToolCall {
                name: assistant_chat_response.choices[0].message.tool_calls[0]
                    .tool_call_function
                    .name
                    .clone(),
                arguments: assistant_chat_response.choices[0].message.tool_calls[0]
                    .tool_call_function
                    .arguments
                    .clone(),
            }
        } else {
            let assistant_message = ChatMessage {
                role: Role::Assistant,
                content: content.clone(),
            };
            self.messages.push(assistant_message);
            AssistantResponse::Message(content)
        };

        Ok(assistant_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_chat_completion_response() {
        let json = r#"{
  "id": "chatcmpl-z9dx813hb6n3ods6u711zf",
  "object": "chat.completion",
  "created": 1777002016,
  "model": "qwen/qwen2.5-coder-14b",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! How can I assist you today?",
        "reasoning_content": "",
        "tool_calls": []
      },
      "logprobs": null,
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 35,
    "completion_tokens": 10,
    "total_tokens": 45,
    "completion_tokens_details": {
      "reasoning_tokens": 0
    }
  },
  "stats": {},
  "system_fingerprint": "qwen/qwen2.5-coder-14b"
}"#;

        let response: ModelResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content,
            "Hello! How can I assist you today?"
        );
    }

    #[tokio::test]
    async fn chat_returns_error_when_server_unreachable() {
        // Point the client at a port nothing is listening on
        let client = RhoHttpClient {
            http_client: reqwest::Client::new(),
            base_url: "http://localhost:9999/v1/chat/completions".to_string(),
        };

        let request = ChatRequest {
            model: "qwen2.5-coder-14b".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: "test".to_string(),
            }],
            tools: vec![],
        };

        let result = client.chat(&request).await;

        assert!(result.is_err(), "expected error when server is unreachable");

        // Optionally check it's the right variant
        match result {
            Err(RhoError::Http(_)) => {} // expected
            Err(other) => panic!("expected Http error, got {other:?}"),
            Ok(_) => panic!("expected error, got success"),
        }
    }
}
