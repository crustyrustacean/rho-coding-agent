//! The [`ChatClient`] trait and [`LocalChatClient`] default implementation.

use crate::config::EgressConfig;
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
///
/// # Egress enforcement
///
/// When constructed with [`LocalChatClient::with_egress`], the client checks
/// the resolved host of the endpoint URL against the egress allowlist before
/// sending any request. Requests to non-allowed hosts are rejected immediately
/// with an error.
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
            Err(crate::error::RhoError::Unexpected(anyhow::anyhow!(
                "egress blocked: host '{host}' is not in the allowed list"
            )))
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
        self.check_egress()?;
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
}
