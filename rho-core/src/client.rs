//! [`RhoAiClient`] — the concrete LLM service client.
//!
//! [`RhoAiClient`] routes to rho-ai's Chat Completions or Responses service
//! and implements [`LlmService`](rho_ai::LlmService). All HTTP communication
//! and SSE parsing is delegated to `rho-ai`.

pub mod error;

use crate::client::error::ClientError;
use crate::config::{ApiProtocol, RhoConfig};
use crate::error::Result;
use async_trait::async_trait;

// ── RhoAiClient ──────────────────────────────────────────────────────────────

/// A [`LlmService`](rho_ai::LlmService) backed by a selected rho-ai transport.
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
    /// instead of deriving one from the generation endpoint. Some providers
    /// (e.g. Z.ai) serve models at a different path prefix.
    models_endpoint: Option<String>,
    /// Wire protocol selected for generation requests.
    protocol: ApiProtocol,
}

impl RhoAiClient {
    /// Create a new client.
    pub fn new(endpoint: impl Into<String>, api_key: Option<String>) -> Self {
        Self::with_protocol(endpoint, api_key, ApiProtocol::ChatCompletions)
    }

    /// Create a new client with an explicit request protocol.
    pub fn with_protocol(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        protocol: ApiProtocol,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            models_endpoint: None,
            protocol,
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
        Self::with_models_endpoint_and_protocol(
            endpoint,
            api_key,
            models_endpoint,
            ApiProtocol::ChatCompletions,
        )
    }

    /// Create a client with explicit model discovery and request protocol.
    pub fn with_models_endpoint_and_protocol(
        endpoint: impl Into<String>,
        api_key: Option<String>,
        models_endpoint: Option<String>,
        protocol: ApiProtocol,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key,
            models_endpoint,
            protocol,
        }
    }

    /// Build rho-ai's shared provider configuration.
    fn service_config(&self) -> rho_ai::ProviderConfig {
        rho_ai::ProviderConfig::new(self.api_key.clone().unwrap_or_default(), &self.endpoint)
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

    /// The selected request protocol.
    #[must_use]
    pub fn protocol(&self) -> ApiProtocol {
        self.protocol
    }

    /// List models available at the server's models endpoint.
    ///
    /// If `models_endpoint` is set, uses that URL directly. Otherwise, derives
    /// the models URL from the configured generation endpoint by replacing a
    /// trailing `/chat/completions` or `/responses` with `/models`. This
    /// preserves provider-specific path prefixes.
    ///
    /// Falls back to `/v1/models` (origin-only) if the endpoint path does not
    /// end with a recognized generation suffix.
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
            if let Some(base) = path
                .strip_suffix("/chat/completions")
                .or_else(|| path.strip_suffix("/responses"))
            {
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
        let config = self.service_config();
        match self.protocol {
            ApiProtocol::ChatCompletions => {
                rho_ai::openai::OpenAiService::new(config)
                    .chat_stream(request)
                    .await
            }
            ApiProtocol::Responses => {
                rho_ai::responses::ResponsesService::new(config)
                    .chat_stream(request)
                    .await
            }
        }
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

    /// Run one local HTTP fixture and return its origin plus captured request.
    fn spawn_http_server(
        response_body: &str,
        content_type: &str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fixture server");
        let address = listener.local_addr().expect("fixture server address");
        let response_body = response_body.to_owned();
        let content_type = content_type.to_owned();
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
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            socket
                .write_all(response.as_bytes())
                .expect("write fixture response");
            String::from_utf8(request).expect("fixture request is UTF-8")
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn legacy_constructors_default_to_chat_completions() {
        let basic = RhoAiClient::new("http://localhost:1234/v1", None);
        let with_models = RhoAiClient::with_models_endpoint(
            "http://localhost:1234/v1",
            None,
            Some("http://localhost:1234/v1/models".to_owned()),
        );
        assert_eq!(basic.protocol(), ApiProtocol::ChatCompletions);
        assert_eq!(with_models.protocol(), ApiProtocol::ChatCompletions);
    }

    #[tokio::test]
    async fn selected_protocol_routes_to_matching_endpoint_and_payload() {
        use futures::StreamExt as _;
        use rho_ai::LlmService as _;

        let fixtures = [
            (
                ApiProtocol::ChatCompletions,
                concat!(
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ),
                "/v1/chat/completions",
                "messages",
            ),
            (
                ApiProtocol::Responses,
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
                "/v1/responses",
                "input",
            ),
        ];

        for (protocol, response, expected_path, expected_field) in fixtures {
            let (origin, request_handle) = spawn_http_server(response, "text/event-stream");
            // Deliberately provide the opposite full endpoint suffix. The
            // selected service must normalize it to its own path.
            let configured_path = if protocol == ApiProtocol::Responses {
                "/v1/chat/completions"
            } else {
                "/v1/responses"
            };
            let client = RhoAiClient::with_protocol(
                format!("{origin}{configured_path}"),
                Some("secret".to_owned()),
                protocol,
            );
            let request = rho_ai::LlmRequest::new(
                "test-model",
                vec![rho_ai::LlmMessage::User("hello".to_owned())],
            );
            let events = client
                .chat_stream(request)
                .await
                .expect("fixture request succeeds")
                .collect::<Vec<_>>()
                .await;
            assert!(events.iter().all(std::result::Result::is_ok));

            let raw = request_handle.join().expect("fixture server joins");
            let (headers, body) = raw.split_once("\r\n\r\n").expect("request has body");
            assert!(headers.starts_with(&format!("POST {expected_path} HTTP/1.1")));
            let json: serde_json::Value = serde_json::from_str(body).expect("JSON request body");
            assert!(json.get(expected_field).is_some());
        }
    }

    #[tokio::test]
    async fn model_discovery_derives_models_from_both_protocol_suffixes() {
        for suffix in ["/v1/chat/completions", "/v1/responses"] {
            let (origin, request_handle) = spawn_http_server(r#"{"data":[]}"#, "application/json");
            let client = RhoAiClient::new(format!("{origin}{suffix}"), None);
            let models = client.list_models().await.expect("models fixture succeeds");
            assert!(models.data().is_empty());
            let raw = request_handle.join().expect("fixture server joins");
            assert!(raw.starts_with("GET /v1/models HTTP/1.1"));
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
