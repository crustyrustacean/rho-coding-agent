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
    /// The endpoint URL (used for display/debugging and constructing requests).
    endpoint: String,
    /// Optional API key (used for authentication and display/debugging).
    api_key: Option<String>,
    /// Optional models endpoint URL, used for model discovery.
    ///
    /// When set, [`list_models`](Self::list_models) uses this URL directly
    /// instead of deriving one from the chat-completions endpoint. Some
    /// providers (e.g. Z.ai) serve the models list at a different path prefix
    /// than chat completions.
    models_endpoint: Option<String>,
}

impl RhoAiClient {
    /// Create a new client.
    pub fn new(endpoint: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            models_endpoint: None,
        }
    }

    /// Create a new client with an explicit models endpoint.
    ///
    /// `models_endpoint` overrides the URL used for model discovery. When
    /// `None`, the models URL is derived from the chat endpoint.
    pub fn with_models_endpoint(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        models_endpoint: Option<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            models_endpoint,
        }
    }

    /// Build an `OpenAiService` for a specific request.
    fn service(&self) -> rho_ai::openai::OpenAiService {
        let api_key = self.api_key.clone().unwrap_or_default();
        let config = rho_ai::ProviderConfig::new(api_key, &self.endpoint);
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

    /// List models available at the server's models endpoint.
    ///
    /// If `models_endpoint` is set, uses that URL directly. Otherwise, derives
    /// the models URL from the configured chat-completions endpoint by
    /// replacing the trailing `/chat/completions` with `/models`. This
    /// preserves any provider-specific path prefix (e.g. `OpenRouter`'s
    /// `/api/v1/…` or `Groq`'s `/openai/v1/…`).
    ///
    /// Falls back to `/v1/models` (origin-only) if the endpoint path
    /// does not end with `/chat/completions`.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint URL cannot be parsed or the request
    /// fails.
    ///
    /// # Panics
    ///
    /// Panics if the `reqwest::Client` builder configuration is invalid.
    pub async fn list_models(&self) -> Result<ModelList> {
        let models_url = if let Some(ref explicit) = self.models_endpoint {
            url::Url::parse(explicit)
                .map_err(|e| crate::error::RhoError::Client(ClientError::UrlParse(e)))?
        } else {
            let mut derived = url::Url::parse(&self.endpoint)
                .map_err(|e| crate::error::RhoError::Client(ClientError::UrlParse(e)))?;
            let path = derived.path();
            if let Some(base) = path.strip_suffix("/chat/completions") {
                derived.set_path(&format!("{base}/models"));
            } else {
                derived.set_path("/v1/models");
            }
            derived
        };
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest Client builder configuration is valid");
        let mut req = client.get(models_url);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        resp.json::<ModelList>().await.map_err(Into::into)
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

// ── Provider bootstrapping ─────────────────────────────────────────────────────

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
///
/// Fields beyond `id` use `#[serde(default)]` to accommodate providers
/// (e.g. `OpenRouter`) that omit `object` and `owned_by` from their response.
///
/// Some providers (e.g. Z.ai) use `slug` instead of `id` as the model
/// identifier. The custom `Deserialize` impl checks `id` first, then falls
/// back to `slug`.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum ModelInfo {
    /// Standard OpenAI-shaped model entry (has `id`).
    Standard {
        /// The model identifier (used in chat completion requests).
        id: String,
        /// The object type (always `"model"`).
        #[serde(default)]
        object: String,
        /// Unix timestamp of creation.
        #[serde(default)]
        created: u64,
        /// Who owns/created this model.
        #[serde(default)]
        owned_by: String,
    },
    /// Non-standard model entry that uses `slug` instead of `id`
    /// (e.g. Z.ai's `{ "slug": "glm-5", ... }`).
    SlugBased {
        /// The model identifier, taken from `slug`.
        #[serde(rename = "slug")]
        id: String,
        /// The object type (always `"model"`).
        #[serde(default)]
        object: String,
        /// Unix timestamp of creation.
        #[serde(default)]
        created: u64,
        /// Who owns/created this model.
        #[serde(default)]
        owned_by: String,
    },
}

impl ModelInfo {
    /// The model identifier (used in chat completion requests).
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Standard { id, .. } | Self::SlugBased { id, .. } => id,
        }
    }

    /// The object type (always `"model"`).
    #[must_use]
    pub fn object(&self) -> &str {
        match self {
            Self::Standard { object, .. } | Self::SlugBased { object, .. } => object,
        }
    }

    /// Unix timestamp of creation.
    #[must_use]
    pub fn created(&self) -> u64 {
        match self {
            Self::Standard { created, .. } | Self::SlugBased { created, .. } => *created,
        }
    }

    /// Who owns/created this model.
    #[must_use]
    pub fn owned_by(&self) -> &str {
        match self {
            Self::Standard { owned_by, .. } | Self::SlugBased { owned_by, .. } => owned_by,
        }
    }
}

/// The response from the `/v1/models` endpoint.
///
/// Most OpenAI-compatible providers return `{ "data": [...] }`. Some
/// providers (e.g. Z.ai) return `{ "models": [...] }` instead. The custom
/// `Deserialize` impl tries `data` first, then falls back to `models`.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum ModelList {
    /// Standard OpenAI-shaped response: `{ "data": [...] }`.
    Standard {
        /// The list of available models.
        data: Vec<ModelInfo>,
    },
    /// Non-standard response with `models` key (e.g. Z.ai).
    ModelsKeyed {
        /// The list of available models.
        models: Vec<ModelInfo>,
    },
}

impl ModelList {
    /// The list of available models, regardless of response shape.
    #[must_use]
    pub fn data(&self) -> &[ModelInfo] {
        match self {
            Self::Standard { data } | Self::ModelsKeyed { models: data } => data,
        }
    }

    /// Consume into the list of available models.
    #[must_use]
    pub fn into_data(self) -> Vec<ModelInfo> {
        match self {
            Self::Standard { data } | Self::ModelsKeyed { models: data } => data,
        }
    }
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
}