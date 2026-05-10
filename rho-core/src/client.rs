//! The [`ChatClient`] trait and [`LocalChatClient`] default implementation.

use crate::error::{Result, RhoError};
use crate::request::ChatRequest;
use crate::response::ModelResponse;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, error, info, warn};

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

/// Default [`ChatClient`] targeting `OpenAI`-compatible endpoints.
///
/// Works with local servers (LM Studio, Ollama) and external providers
/// (`OpenRouter`, `OpenAI`, `DeepInfra`, `Groq`, etc.) — anything that speaks
/// the `OpenAI` wire format.
///
/// # Bearer authentication
///
/// When an API key is provided, it is sent as an `Authorization: Bearer`
/// header with every request. This is required for external providers but
/// unused for local endpoints.
#[derive(Clone, Debug)]
pub struct LocalChatClient {
    /// The underlying HTTP client.
    http_client: Client,
    /// The model API endpoint URL.
    endpoint: String,
    /// Optional API key for bearer authentication.
    ///
    /// When `Some`, sent as `Authorization: Bearer <key>` with each request.
    /// Local endpoints typically don't need this.
    api_key: Option<String>,
}

impl LocalChatClient {
    /// Create a client at the default local endpoint
    /// (`http://localhost:1234/v1/chat/completions`).
    ///
    /// No egress enforcement is applied — all hosts are permitted.
    /// This is the legacy constructor for backward compatibility.
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            endpoint: "http://localhost:1234/v1/chat/completions".to_owned(),
            api_key: None,
        }
    }

    /// Create a client at a custom endpoint URL.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
            api_key: None,
        }
    }

    /// Create a client at a custom endpoint URL with optional bearer
    /// authentication.
    pub fn with_endpoint_and_key(endpoint: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
            api_key,
        }
    }

    /// List models available at the server's `/v1/models` endpoint.
    ///
    /// Derives the models URL from the configured completions endpoint by
    /// replacing the `/v1/chat/completions` path with `/v1/models`. Uses
    /// URL parsing so trailing slashes and non-standard paths are handled.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint URL cannot be parsed or the request
    /// fails (e.g. the server is unreachable).
    pub async fn list_models(&self) -> Result<ModelList> {
        let models_url = reqwest::Url::parse(&self.endpoint)
            .map(|mut u| {
                u.set_path("/v1/models");
                u
            })
            .map_err(|e| RhoError::Unexpected(anyhow::anyhow!("bad endpoint URL: {e}")))?;
        let mut req = self.http_client.get(models_url);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }
        Ok(req.send().await?.json::<ModelList>().await?)
    }
}

impl Default for LocalChatClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A model returned by the `/v1/models` endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelInfo {
    /// The model identifier (used in chat completion requests).
    pub id: String,
    /// The object type (always `"model"`).
    pub object: String,
    /// Unix timestamp of creation.
    #[serde(default)]
    pub created: u64,
    /// Who owns/created this model.
    #[serde(default)]
    pub owned_by: String,
}

/// The response from the `/v1/models` endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelList {
    /// The list of available models.
    pub data: Vec<ModelInfo>,
}

/// Truncate a response body for inclusion in error messages.
///
/// Uses `floor_char_boundary` which requires Rust ≥ 1.82.
fn truncate_error_body(body: &str) -> &str {
    const MAX_LEN: usize = 512;
    if body.len() <= MAX_LEN {
        body
    } else {
        &body[..body.floor_char_boundary(MAX_LEN)]
    }
}

/// Build a human-readable message for a non-2xx HTTP response body.
///
/// Includes actionable suggestions for known error patterns (e.g. context
/// window exceeded from llama.cpp / LM Studio).
fn enhance_http_body(status: u16, body: &str) -> String {
    let snippet = truncate_error_body(body);

    // Detect the common "context window exceeded" error from llama.cpp / LM Studio.
    if status == 400 && body.contains("n_keep") && body.contains("n_ctx") {
        return format!(
            "context window exceeded.\
             \n  The system prompt + tool schemas exceed the model's context length.\
             \n  Try one of:\
             \n    1. Load the model with a larger context length in LM Studio\
             \n    2. Use --compact to send a shorter system prompt\
             \n    3. Use a model with a larger context window\
             \n  Server details: {snippet}"
        );
    }

    snippet.to_string()
}

#[async_trait]
impl ChatClient for LocalChatClient {
    #[tracing::instrument(skip_all, fields(model = %request.model, message_count = request.messages.len(), tool_count = request.tools.len()))]
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse> {
        debug!(
            model = %request.model,
            num_messages = request.messages.len(),
            num_tools = request.tools.len(),
            "sending chat request"
        );
        let mut request_builder = self.http_client.post(&self.endpoint).json(&request);
        if let Some(ref key) = self.api_key {
            request_builder = request_builder.bearer_auth(key);
        }
        let response = request_builder.send().await?;

        let status = response.status();

        // Read the body as text so we can report it on parse failures and
        // include it in HTTP error diagnostics. If the body read itself
        // fails (e.g. connection dropped mid-response), propagate as
        // RhoError::Http so it remains retryable.
        let body = response.text().await?;

        if !status.is_success() {
            // Non-2xx HTTP response. Use HttpError which preserves the
            // status code for retry classification.
            warn!(status = status.as_u16(), body = %truncate_error_body(&body));
            return Err(RhoError::HttpError {
                status: status.as_u16(),
                message: enhance_http_body(status.as_u16(), &body),
            });
        }

        let model_response = serde_json::from_str::<ModelResponse>(&body).map_err(|e| {
            error!(error = %e);
            RhoError::Unexpected(anyhow::anyhow!(
                "failed to parse model response: {e}\n  raw response (first 512 chars): {}",
                truncate_error_body(&body)
            ))
        })?;

        // Log response telemetry for data model assessment.
        if let Some(choice) = model_response.choices.first() {
            info!(
                finish_reason = ?choice.finish_reason,
                prompt_tokens = model_response.usage.prompt_tokens,
                completion_tokens = model_response.usage.completion_tokens,
                total_tokens = model_response.usage.total_tokens,
            );
        }

        Ok(model_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Endpoint derivation ──────────────────────────────────────────────

    #[test]
    fn default_endpoint_derives_models_url() {
        let client = LocalChatClient::new();
        let models_url = reqwest::Url::parse(&client.endpoint).unwrap();
        let mut expected = models_url.clone();
        expected.set_path("/v1/models");
        assert_eq!(expected.as_str(), "http://localhost:1234/v1/models");
    }

    #[test]
    fn custom_endpoint_derives_models_url() {
        let client = LocalChatClient::with_endpoint("http://localhost:8080/v1/chat/completions");
        let models_url = reqwest::Url::parse(&client.endpoint).unwrap();
        let mut expected = models_url.clone();
        expected.set_path("/v1/models");
        assert_eq!(expected.as_str(), "http://localhost:8080/v1/models");
    }

    #[test]
    fn trailing_slash_endpoint_still_derives_models_url() {
        let client = LocalChatClient::with_endpoint("http://localhost:1234/v1/chat/completions/");
        let models_url = reqwest::Url::parse(&client.endpoint).unwrap();
        let mut expected = models_url.clone();
        expected.set_path("/v1/models");
        assert_eq!(expected.as_str(), "http://localhost:1234/v1/models");
    }
}
