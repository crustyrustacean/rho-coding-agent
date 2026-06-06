//! [`RhoAiClient`] — the concrete LLM service client.
//!
//! [`RhoAiClient`] wraps [`rho_ai::openai::OpenAiService`] and implements
//! [`LlmService`](rho_ai::LlmService). All HTTP communication and SSE parsing
//! is delegated to `rho-ai`.

pub mod error;

use crate::client::error::ClientError;
use crate::config::RhoConfig;
use crate::error::Result;
use async_trait::async_trait;

// ── RhoAiClient ──────────────────────────────────────────────────────────────

/// A [`LlmService`](rho_ai::LlmService) backed by [`rho_ai::openai::OpenAiService`].
///
/// This is the sole client implementation. It delegates all HTTP communication
/// and SSE parsing to `rho-ai`, adapting between rho-core's types and rho-ai's
/// unified types at the boundary.
#[derive(Clone, Debug)]
pub struct RhoAiClient {
    /// The model identifier.
    model: String,
    /// The endpoint URL (used for display/debugging).
    endpoint: String,
    /// Optional API key (used for display/debugging).
    api_key: Option<String>,
}

impl RhoAiClient {
    /// Create a new client.
    pub fn new(
        model: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            model: model.into(),
            endpoint: endpoint.into(),
            api_key,
        }
    }

    /// Build an `OpenAiService` for a specific request.
    fn service(&self) -> rho_ai::openai::OpenAiService {
        let api_key = self.api_key.clone().unwrap_or_default();
        let config = rho_ai::ProviderConfig::new(&self.model, api_key, &self.endpoint);
        rho_ai::openai::OpenAiService::new(config)
    }

    /// The configured endpoint URL.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The configured API key (if any).
    #[must_use]
    pub fn api_key(&self) -> &Option<String> {
        &self.api_key
    }

    /// List models available at the server's `/v1/models` endpoint.
    ///
    /// Derives the models URL from the configured endpoint by
    /// replacing the path with `/v1/models`.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint URL cannot be parsed or the request
    /// fails.
    pub async fn list_models(&self) -> Result<ModelList> {
        let mut models_url = url::Url::parse(&self.endpoint)
            .map_err(|e| crate::error::RhoError::Client(ClientError::UrlParse(e)))?;
        models_url.set_path("/v1/models");
        let client = reqwest::Client::new();
        let mut req = client.get(models_url);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }
        Ok(req.send().await?.json::<ModelList>().await?)
    }
}

impl Default for RhoAiClient {
    fn default() -> Self {
        Self::new("default", DEFAULT_ENDPOINT, None)
    }
}

// ── LlmService impl ──────────────────────────────────────────────────────────

#[async_trait]
impl rho_ai::LlmService for RhoAiClient {
    async fn chat_stream(
        &self,
        request: rho_ai::types::LlmRequest,
    ) -> std::result::Result<rho_ai::EventStream, rho_ai::ProviderError> {
        let service = self.service();
        service.chat_stream(request).await
    }
}

// ── Shared bootstrapping ─────────────────────────────────────────────────────

/// The default endpoint URL when no override or config is set.
const DEFAULT_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";

/// Construct a fully-configured [`RhoAiClient`] from [`RhoConfig`].
///
/// **Prefer [`provider_factory`](crate::provider_factory) for new code** — it
/// returns a [`Box<dyn Provider>`](crate::Provider) that encapsulates client
/// construction, model discovery, and externality checking.
///
/// This function remains available for:
/// - Bench harnesses that need a concrete client
/// - Tests that bypass the provider abstraction
/// - Backward compatibility
///
/// Reads `provider.endpoint` and `provider.api_key_env` from config.
/// CLI overrides for endpoint and api-key-env are applied on top.
///
/// Priority (endpoint): CLI override → config → default.
/// Priority (api key): CLI override → config `provider.api_key_env`.
pub fn client_factory(
    config: &RhoConfig,
    endpoint_override: Option<&str>,
    api_key_env_override: Option<&str>,
) -> RhoAiClient {
    let endpoint = endpoint_override
        .map(String::from)
        .or_else(|| config.provider.default_endpoint().map(String::from))
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

    let api_key = resolve_api_key(config, api_key_env_override);

    // Extract model from the endpoint's base URL or use a default.
    // The model will be overridden per-request via ChatRequest.model.
    RhoAiClient::new("default", endpoint, api_key)
}

/// Resolve the API key from provider configuration.
///
/// CLI `--api-key-env` takes priority over config `provider.api_key_env`.
/// Reads the named environment variable and returns the value.
/// Returns `None` if no env var is configured or the variable is not set.
pub fn resolve_api_key(config: &RhoConfig, api_key_env_override: Option<&str>) -> Option<String> {
    let env_var = api_key_env_override.or(config.provider.default_api_key_env())?;
    let key = std::env::var(env_var).ok()?;
    if key.is_empty() { None } else { Some(key) }
}

/// Determine whether an endpoint URL points to a local address.
///
/// A local endpoint is one whose host is `localhost`, `127.0.0.1`, or `::1`.
/// Any other host is considered external.
///
/// Uses `url::Url` parsing so that crafted hostnames like
/// `api.localhost-fake.evil.com` are correctly classified as external.
pub fn is_local_endpoint(endpoint: &str) -> bool {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .is_some_and(|h| matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"))
}

// ── Legacy types for backward compatibility ──────────────────────────────────

/// A model returned by the `/v1/models` endpoint.
#[derive(Clone, Debug, serde::Deserialize)]
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
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ModelList {
    /// The list of available models.
    pub data: Vec<ModelInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderConfig, ProviderSettings};

    /// Helper: create an `RhoConfig` with a single provider having
    /// only the given field set (everything else default).
    fn config_with_provider(field: &str, value: String) -> RhoConfig {
        let pc = match field {
            "endpoint" => ProviderConfig {
                endpoint: Some(value),
                ..Default::default()
            },
            "api_key_env" => ProviderConfig {
                api_key_env: Some(value),
                ..Default::default()
            },
            _ => ProviderConfig::default(),
        };
        RhoConfig {
            provider: ProviderSettings {
                providers: vec![pc],
            },
            ..Default::default()
        }
    }

    // ── client_factory ───────────────────────────────────────────────────

    #[test]
    fn client_factory_defaults_when_no_config_or_override() {
        let config = RhoConfig::default();
        let client = client_factory(&config, None, None);
        assert_eq!(client.endpoint(), DEFAULT_ENDPOINT);
        assert!(client.api_key().is_none());
    }

    #[test]
    fn client_factory_uses_endpoint_override() {
        let config = RhoConfig::default();
        let client = client_factory(
            &config,
            Some("http://example.com/v1/chat/completions"),
            None,
        );
        assert_eq!(client.endpoint(), "http://example.com/v1/chat/completions");
    }

    #[test]
    fn client_factory_override_beats_config() {
        let config =
            config_with_provider("endpoint", "http://config.com/v1/chat/completions".into());
        let client = client_factory(
            &config,
            Some("http://override.com/v1/chat/completions"),
            None,
        );
        assert_eq!(client.endpoint(), "http://override.com/v1/chat/completions");
    }

    #[test]
    fn client_factory_uses_config_endpoint() {
        let config =
            config_with_provider("endpoint", "http://config.com/v1/chat/completions".into());
        let client = client_factory(&config, None, None);
        assert_eq!(client.endpoint(), "http://config.com/v1/chat/completions");
    }

    #[test]
    fn client_factory_uses_config_api_key() {
        let config = config_with_provider("api_key_env", "RHO_TEST_API_KEY_12345".into());
        temp_env::with_var("RHO_TEST_API_KEY_12345", Some("test-key-value"), || {
            let client = client_factory(&config, None, None);
            assert_eq!(client.api_key().as_deref(), Some("test-key-value"));
        });
    }

    #[test]
    fn client_factory_api_key_override_beats_config() {
        let config = config_with_provider("api_key_env", "CONFIG_KEY".into());
        temp_env::with_vars(
            [
                ("CONFIG_KEY", Some("config-key")),
                ("OVERRIDE_KEY", Some("override-key")),
            ],
            || {
                let client = client_factory(&config, None, Some("OVERRIDE_KEY"));
                assert_eq!(client.api_key().as_deref(), Some("override-key"));
            },
        );
    }

    // ── resolve_api_key ──────────────────────────────────────────────────

    #[test]
    fn resolve_api_key_returns_none_when_nothing_configured() {
        let config = RhoConfig::default();
        assert!(resolve_api_key(&config, None).is_none());
    }

    #[test]
    fn resolve_api_key_reads_from_config() {
        let config = config_with_provider("api_key_env", "RHO_TEST_KEY_RESOLVE".into());
        temp_env::with_var("RHO_TEST_KEY_RESOLVE", Some("secret"), || {
            assert_eq!(resolve_api_key(&config, None), Some("secret".to_owned()));
        });
    }

    #[test]
    fn resolve_api_key_override_beats_config() {
        let config = config_with_provider("api_key_env", "CONFIG_ENV".into());
        temp_env::with_vars(
            [
                ("CONFIG_ENV", Some("config-val")),
                ("CLI_ENV", Some("cli-val")),
            ],
            || {
                assert_eq!(
                    resolve_api_key(&config, Some("CLI_ENV")),
                    Some("cli-val".to_owned())
                );
            },
        );
    }

    #[test]
    fn resolve_api_key_returns_none_for_empty_value() {
        let config = config_with_provider("api_key_env", "RHO_TEST_EMPTY_KEY".into());
        temp_env::with_var("RHO_TEST_EMPTY_KEY", Some(""), || {
            assert!(resolve_api_key(&config, None).is_none());
        });
    }

    // ── is_local_endpoint ────────────────────────────────────────────────

    #[test]
    fn local_endpoint_localhost() {
        assert!(is_local_endpoint(
            "http://localhost:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_127_0_0_1() {
        assert!(is_local_endpoint(
            "http://127.0.0.1:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_ipv6_loopback() {
        assert!(is_local_endpoint("http://[::1]:1234/v1/chat/completions"));
    }

    #[test]
    fn external_endpoint_openai() {
        assert!(!is_local_endpoint(
            "https://api.openai.com/v1/chat/completions"
        ));
    }

    #[test]
    fn external_endpoint_anthropic() {
        assert!(!is_local_endpoint("https://api.anthropic.com/v1/messages"));
    }

    #[test]
    fn local_endpoint_case_insensitive() {
        assert!(is_local_endpoint(
            "http://LocalHost:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_rejects_localhost_subdomain() {
        assert!(!is_local_endpoint(
            "https://api.localhost-fake.evil.com/v1/chat/completions"
        ));
    }

    // ── Message conversion ───────────────────────────────────────────────

    // ── Tool conversion ──────────────────────────────────────────────────

    // ── content_into_string ──────────────────────────────────────────────
}
