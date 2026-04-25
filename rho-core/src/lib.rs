//! Core library for rho-coding-agent.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Domain types - request

#[derive(Clone, Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Domain types - response

#[derive(Clone, Debug, Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub object: String,
    pub created: usize,
    pub model: String,
    pub choices: Vec<ModelChoice>,
    pub usage: ModelUsage,
    pub stats: ModelStats,
    pub system_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelStats {}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelChoice {
    pub index: usize,
    pub message: ModelMessage,
    pub logprobs: Option<String>,
    pub finish_reason: FinishReason,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelMessage {
    pub role: Role,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<ModelToolCalls>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelToolCalls {
    #[serde(rename = "type")]
    pub tool_call_type: String,
    pub id: String,
    #[serde(rename = "function")]
    pub tool_call_function: ToolCallFunction,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub completion_tokens_details: ReasoningTokens,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReasoningTokens {
    pub reasoning_tokens: usize,
}

/// Error type

#[derive(Debug, Error)]
pub enum RhoError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Unexpected(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, RhoError>;

/// HTTP client
pub struct RhoHttpClient {
    /// the reqwest HTTP client
    http_client: Client,
    /// the model API url
    base_url: String,
}

impl RhoHttpClient {
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            base_url: "http://localhost:1234/v1/chat/completions".to_string(),
        }
    }

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
