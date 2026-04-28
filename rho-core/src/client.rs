//! The [`ChatClient`] trait and [`LocalChatClient`] default implementation.

use crate::error::Result;
use crate::request::ChatRequest;
use crate::response::ModelResponse;
use async_trait::async_trait;
use reqwest::Client;

/// Interface all model providers must implement.
///
/// # Dyn-compatibility
///
/// `#[async_trait]` is required because the binary swaps providers at runtime
/// (`/provider`), which requires `Box<dyn ChatClient>`. Native AFIT is not
/// dyn-compatible.
#[async_trait]
pub trait ChatClient: Send + Sync {
    /// Send a chat completion request and return the model's response.
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse>;
}

/// Default [`ChatClient`] targeting `localhost` OpenAI-compatible endpoints
/// (LM Studio, Ollama, etc.).
///
/// Replaces the earlier `RhoHttpClient`.
pub struct LocalChatClient {
    /// The underlying HTTP client.
    http_client: Client,
    /// The model API endpoint URL.
    endpoint: String,
}

impl LocalChatClient {
    /// Create a client at the default local endpoint
    /// (`http://localhost:1234/v1/chat/completions`).
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            endpoint: "http://localhost:1234/v1/chat/completions".to_owned(),
        }
    }

    /// Create a client at a custom endpoint URL.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
        }
    }
}

impl Default for LocalChatClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChatClient for LocalChatClient {
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse> {
        Ok(self
            .http_client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await?
            .json::<ModelResponse>()
            .await?)
    }
}
