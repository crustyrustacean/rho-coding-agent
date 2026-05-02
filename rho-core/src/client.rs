//! The [`ChatClient`] trait and [`LocalChatClient`] default implementation.

use crate::config::EgressConfig;
use crate::error::{Result, RhoError};
use crate::request::ChatRequest;
use crate::response::ModelResponse;
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::{error, info, warn};

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
///
/// # Egress enforcement
///
/// When constructed with [`LocalChatClient::with_egress`], the client checks
/// the resolved host of the endpoint URL against the egress allowlist before
/// sending any request. Requests to non-allowed hosts are rejected immediately
/// with an error.
#[derive(Clone, Debug)]
pub struct LocalChatClient {
    /// The underlying HTTP client.
    http_client: Client,
    /// The model API endpoint URL.
    endpoint: String,
    /// Egress allowlist. When `None`, all hosts are permitted (legacy mode).
    egress: Option<EgressConfig>,
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
            egress: None,
        }
    }

    /// Create a client at a custom endpoint URL.
    ///
    /// No egress enforcement is applied — all hosts are permitted.
    /// Use [`LocalChatClient::with_endpoint_and_egress`] to enforce the
    /// egress allowlist.
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
            egress: None,
        }
    }

    /// Create a client at a custom endpoint URL with egress enforcement.
    ///
    /// Before each request, the host portion of the endpoint URL is checked
    /// against the egress allowlist. Requests to non-allowed hosts are
    /// rejected with an error.
    pub fn with_endpoint_and_egress(endpoint: impl Into<String>, egress: EgressConfig) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
            egress: Some(egress),
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
        self.check_egress()?;
        let models_url = reqwest::Url::parse(&self.endpoint)
            .map(|mut u| {
                u.set_path("/v1/models");
                u
            })
            .map_err(|e| RhoError::Unexpected(anyhow::anyhow!("bad endpoint URL: {e}")))?;
        Ok(self
            .http_client
            .get(models_url)
            .send()
            .await?
            .json::<ModelList>()
            .await?)
    }

    /// Check whether the configured endpoint host is permitted by the egress
    /// allowlist.
    ///
    /// Returns `Ok(())` if the host is allowed or if no egress config is set.
    /// Returns an error with the blocked host name if the host is not allowed.
    fn check_egress(&self) -> Result<()> {
        let Some(egress) = &self.egress else {
            return Ok(());
        };

        let host = reqwest::Url::parse(&self.endpoint)
            .ok()
            .and_then(|url| url.host_str().map(String::from));

        let Some(host) = host else {
            return Ok(());
        };

        if egress.is_host_allowed(&host) {
            Ok(())
        } else {
            Err(crate::error::RhoError::EgressBlocked { host: host.clone() })
        }
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
    #[tracing::instrument(skip(request), fields(model = %request.model, message_count = request.messages.len(), tool_count = request.tools.len()))]
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse> {
        self.check_egress()?;
        let response = self
            .http_client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await?;

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
            let reasoning_tokens = model_response
                .usage
                .completion_tokens_details
                .as_ref()
                .map_or(0, |d| d.reasoning_tokens);
            info!(
                finish_reason = ?choice.finish_reason,
                prompt_tokens = model_response.usage.prompt_tokens,
                completion_tokens = model_response.usage.completion_tokens,
                total_tokens = model_response.usage.total_tokens,
                has_reasoning = !choice.message.reasoning_content.is_empty(),
                reasoning_tokens
            );
        }

        Ok(model_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Egress enforcement ───────────────────────────────────────────────

    #[test]
    fn check_egress_allows_localhost_without_config() {
        let client = LocalChatClient::new();
        assert!(client.check_egress().is_ok());
    }

    #[test]
    fn check_egress_allows_localhost_with_empty_egress() {
        let client = LocalChatClient::with_endpoint_and_egress(
            "http://localhost:1234/v1/chat/completions",
            EgressConfig::default(),
        );
        assert!(client.check_egress().is_ok());
    }

    #[test]
    fn check_egress_allows_127_0_0_1() {
        let client = LocalChatClient::with_endpoint_and_egress(
            "http://127.0.0.1:1234/v1/chat/completions",
            EgressConfig::default(),
        );
        assert!(client.check_egress().is_ok());
    }

    #[test]
    fn check_egress_blocks_unknown_host_by_default() {
        let client = LocalChatClient::with_endpoint_and_egress(
            "https://api.openai.com/v1/chat/completions",
            EgressConfig::default(),
        );
        let err = client.check_egress().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("egress blocked"),
            "expected egress blocked message, got: {msg}"
        );
        assert!(
            msg.contains("api.openai.com"),
            "expected host in error, got: {msg}"
        );
    }

    #[test]
    fn check_egress_allows_listed_host() {
        let egress = EgressConfig {
            allowed_hosts: vec!["api.openai.com".to_owned()],
        };
        let client = LocalChatClient::with_endpoint_and_egress(
            "https://api.openai.com/v1/chat/completions",
            egress,
        );
        assert!(client.check_egress().is_ok());
    }

    #[test]
    fn check_egress_blocks_unlisted_host_even_when_others_allowed() {
        let egress = EgressConfig {
            allowed_hosts: vec!["api.openai.com".to_owned()],
        };
        let client = LocalChatClient::with_endpoint_and_egress(
            "https://api.anthropic.com/v1/messages",
            egress,
        );
        assert!(client.check_egress().is_err());
    }

    #[test]
    fn check_egress_no_config_allows_any_host() {
        // Without egress config, any host is permitted.
        let client = LocalChatClient::with_endpoint("https://api.openai.com/v1/chat/completions");
        assert!(client.check_egress().is_ok());
    }

    // ── list_models egress ───────────────────────────────────────────────

    #[tokio::test]
    async fn list_models_blocks_external_host_by_default() {
        let client = LocalChatClient::with_endpoint_and_egress(
            "https://api.openai.com/v1/chat/completions",
            EgressConfig::default(),
        );
        let err = client.list_models().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("egress blocked"),
            "expected egress blocked message, got: {msg}"
        );
    }

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
