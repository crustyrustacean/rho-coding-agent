//! The [`ChatClient`] trait and [`LocalChatClient`] default implementation.
//!
//! Also provides [`client_factory`] for constructing a fully-configured client
//! from [`RhoConfig`] with optional CLI overrides.

use crate::config::RhoConfig;
use crate::error::{Result, RhoError};
use crate::request::ChatRequest;
use crate::response::{FinishReason, ModelResponse};
use crate::stream::StreamChunk;
use async_trait::async_trait;
use futures::stream::Stream;
use reqwest::Client;
use serde::Deserialize;
use std::pin::Pin;
use tracing::{debug, error, info, warn};

/// The type returned by [`ChatClient::chat_stream`].
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

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

    /// Send a chat completion request and receive a stream of chunks.
    ///
    /// The default implementation wraps [`chat`](Self::chat) — it sends a
    /// non-streaming request and converts the full response into a `Vec`
    /// of [`StreamChunk`] via [`StreamChunk::from_response`]. This means
    /// every [`ChatClient`] implementation supports streaming automatically;
    /// providers that natively support SSE override this method for
    /// incremental token delivery.
    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        let response = self.chat(request).await?;
        let chunks = StreamChunk::from_response(&response);
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
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
    /// This is a convenience constructor equivalent to
    /// `with_endpoint(DEFAULT_ENDPOINT)`. For custom endpoints, use
    /// [`with_endpoint()`]. For production use with config, prefer
    /// [`client_factory()`].
    pub fn new() -> Self {
        Self::with_endpoint(DEFAULT_ENDPOINT)
    }

    /// Create a client at a custom endpoint URL.
    ///
    /// # When to use
    ///
    /// - Local server at non-default port or path
    /// - Quick configuration without loading config files
    ///
    /// # When not to use
    ///
    /// - Production with config: use [`client_factory()`]
    /// - Need API key authentication: use [`with_endpoint_and_key()`]
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: endpoint.into(),
            api_key: None,
        }
    }

    /// Create a client at a custom endpoint URL with optional bearer
    /// authentication.
    ///
    /// # When to use
    ///
    /// - External API providers (`OpenRouter`, `OpenAI`, etc.)
    /// - Quick configuration without loading config files
    ///
    /// # When not to use
    ///
    /// - Production with config: use [`client_factory()`]
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

// ── SSE wire types (private) ──────────────────────────────────────────────────
//
// These types mirror the OpenAI SSE wire format. They are only used by
// `LocalChatClient::chat_stream` to deserialize individual `data:` lines.
// Clippy's `missing_docs_in_private_items` lint is suppressed for these
// deserialization-only structs.

/// A single SSE event payload from the streaming API.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::missing_docs_in_private_items)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<SseChoice>,
}

/// A single choice within an SSE chunk.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::missing_docs_in_private_items)]
struct SseChoice {
    delta: SseDelta,
    finish_reason: Option<FinishReason>,
}

/// The delta content within an SSE choice.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(clippy::missing_docs_in_private_items)]
struct SseDelta {
    #[serde(default)]
    #[allow(dead_code)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<SseToolCallDelta>>,
}

/// A tool call delta within an SSE choice.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::missing_docs_in_private_items)]
struct SseToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<SseFunctionDelta>,
}

/// A function delta within a tool call delta.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(clippy::missing_docs_in_private_items)]
struct SseFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
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

    async fn chat_stream(&self, request: ChatRequest) -> Result<ChatStream> {
        info!("sending streaming request to {}", self.endpoint);
        let mut request_builder = self.http_client.post(&self.endpoint).json(&request);
        if let Some(ref key) = self.api_key {
            request_builder = request_builder.bearer_auth(key);
        }

        let response = request_builder.send().await?;
        let status = response.status();
        info!("received response with status: {}", status);
        
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "unable to read error body".to_string());
            error!("streaming request failed with status {}: {}", status, error_text);
            return Err(RhoError::HttpError {
                status: status.as_u16(),
                message: error_text,
            });
        }

        let byte_stream = response.bytes_stream();
        debug!("created byte stream from response");

        // Build a `futures::Stream` that buffers SSE lines and emits
        // `StreamChunk` items.
        let stream = SseStream::new(Box::pin(byte_stream));
        Ok(Box::pin(stream))
    }
}

// ── SSE line-buffered stream ──────────────────────────────────────────────────

/// A `futures::Stream` that consumes a reqwest byte stream, buffers SSE
/// lines, and emits parsed [`StreamChunk`] items.
///
/// SSE data may be split across TCP frames arbitrarily, so we must buffer
/// partial lines and only process complete `\n`-terminated lines.
struct SseStream {
    /// Inner byte stream from reqwest.
    byte_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>,
    >,
    /// Line buffer for accumulating partial SSE lines across chunks.
    line_buf: String,
    /// Whether `[DONE]` has been received.
    done: bool,
}

impl SseStream {
    /// Create a new SSE stream parser wrapping a reqwest byte stream.
    #[allow(clippy::missing_docs_in_private_items)]
    fn new(
        byte_stream: std::pin::Pin<
            Box<
                dyn futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>>
                    + Send,
            >,
        >,
    ) -> Self {
        Self {
            byte_stream,
            line_buf: String::new(),
            done: false,
        }
    }

    /// Try to extract the next `StreamChunk` from the line buffer.
    /// Returns `None` if no complete SSE event is available.
    fn try_next_chunk(&mut self) -> Option<Result<StreamChunk>> {
        loop {
            if self.done {
                return None;
            }

            // Find the next complete line.
            let newline_pos = self.line_buf.find('\n')?;

            let line = self.line_buf[..newline_pos]
                .trim_end_matches('\r')
                .to_owned();
            self.line_buf = self.line_buf[newline_pos + 1..].to_owned();

            // Skip non-data lines.
            let Some(payload) = line.strip_prefix("data: ") else {
                continue;
            };
            let payload = payload.trim();

            // Check for stream end sentinel.
            if payload == "[DONE]" {
                self.done = true;
                return None;
            }

            // Parse the JSON payload.
            let sse_chunk = match serde_json::from_str::<SseChunk>(payload) {
                Ok(c) => c,
                Err(e) => {
                    warn!("failed to parse SSE chunk: {e}; payload: {payload}");
                    continue;
                }
            };

            // Convert the first choice into StreamChunk(s).
            if let Some(choice) = sse_chunk.choices.first() {
                // Emit text delta.
                if let Some(text) = choice.delta.content.clone() {
                    return Some(Ok(StreamChunk::TextDelta(text)));
                }
                // Emit reasoning delta.
                if let Some(reasoning) = choice.delta.reasoning_content.clone() {
                    return Some(Ok(StreamChunk::ReasoningDelta(reasoning)));
                }
                // Emit tool call deltas.
                if let Some(tool_call_deltas) = &choice.delta.tool_calls
                    && let Some(tc_delta) = tool_call_deltas.first()
                {
                    let func = tc_delta.function.as_ref();
                    return Some(Ok(StreamChunk::ToolCallDelta {
                        index: tc_delta.index,
                        id: tc_delta.id.clone(),
                        function_name: func.and_then(|f| f.name.clone()),
                        arguments_delta: func.and_then(|f| f.arguments.clone()),
                    }));
                }
                // Emit done.
                if let Some(reason) = &choice.finish_reason {
                    return Some(Ok(StreamChunk::Done(reason.clone())));
                }
            }
            // If no useful data in this SSE event, continue to next line.
            debug!("SSE chunk had no text, reasoning, tool_calls, or finish_reason; skipping");
        }
    }
}

impl futures::Stream for SseStream {
    type Item = Result<StreamChunk>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        debug!("poll_next called: done={}, line_buf_len={}", self.done, self.line_buf.len());
        // First, try to extract a chunk from already-buffered lines.
        if let Some(chunk) = self.try_next_chunk() {
            return std::task::Poll::Ready(Some(chunk));
        }

        // If done, close the stream.
        if self.done {
            return std::task::Poll::Ready(None);
        }

        // Poll the inner byte stream for more data.
        loop {
            match self.byte_stream.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(bytes))) => {
                    let bytes_str = String::from_utf8_lossy(&bytes).to_string();
                    debug!("received {} bytes from byte stream: {:?}", bytes.len(), bytes_str);
                    self.line_buf.push_str(&bytes_str);
                    if let Some(chunk) = self.try_next_chunk() {
                        return std::task::Poll::Ready(Some(chunk));
                    }
                    // try_next_chunk may have set `done`; check before
                    // polling again.
                    if self.done {
                        return std::task::Poll::Ready(None);
                    }
                    // No chunk ready yet — keep polling.
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    warn!("SSE byte stream error: {e}");
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Ready(None) => {
                    // Inner stream exhausted.
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

// ── Shared bootstrapping ─────────────────────────────────────────────────────

/// The default endpoint URL when no override or config is set.
const DEFAULT_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";

/// Construct a fully-configured [`LocalChatClient`] from [`RhoConfig`].
///
/// This is the **recommended** way to construct clients in production.
/// It respects config values, handles API key resolution from environment
/// variables, and applies CLI overrides.
///
/// For quick testing without config, you may use [`new()`], [`with_endpoint()`],
/// or [`with_endpoint_and_key()`] directly.
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
) -> LocalChatClient {
    let endpoint = endpoint_override
        .map(String::from)
        .or_else(|| config.provider.endpoint.clone())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

    let api_key = resolve_api_key(config, api_key_env_override);

    match api_key {
        Some(key) => LocalChatClient::with_endpoint_and_key(endpoint, Some(key)),
        None => LocalChatClient::with_endpoint(endpoint),
    }
}

/// Resolve the API key from provider configuration.
///
/// CLI `--api-key-env` takes priority over config `provider.api_key_env`.
/// Reads the named environment variable and returns the value.
/// Returns `None` if no env var is configured or the variable is not set.
pub fn resolve_api_key(config: &RhoConfig, api_key_env_override: Option<&str>) -> Option<String> {
    let env_var = api_key_env_override.or(config.provider.api_key_env.as_deref())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── client_factory ───────────────────────────────────────────────────────

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
        let mut config = RhoConfig::default();
        config.provider.endpoint = Some("http://config.com/v1/chat/completions".to_owned());
        let client = client_factory(
            &config,
            Some("http://override.com/v1/chat/completions"),
            None,
        );
        assert_eq!(client.endpoint(), "http://override.com/v1/chat/completions");
    }

    #[test]
    fn client_factory_uses_config_endpoint() {
        let mut config = RhoConfig::default();
        config.provider.endpoint = Some("http://config.com/v1/chat/completions".to_owned());
        let client = client_factory(&config, None, None);
        assert_eq!(client.endpoint(), "http://config.com/v1/chat/completions");
    }

    #[test]
    fn client_factory_uses_config_api_key() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("RHO_TEST_API_KEY_12345".to_owned());
        temp_env::with_var("RHO_TEST_API_KEY_12345", Some("test-key-value"), || {
            let client = client_factory(&config, None, None);
            assert_eq!(client.api_key().as_deref(), Some("test-key-value"));
        });
    }

    #[test]
    fn client_factory_api_key_override_beats_config() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("CONFIG_KEY".to_owned());
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

    // ── resolve_api_key ────────────────────────────────────────────────────

    #[test]
    fn resolve_api_key_returns_none_when_nothing_configured() {
        let config = RhoConfig::default();
        assert!(resolve_api_key(&config, None).is_none());
    }

    #[test]
    fn resolve_api_key_reads_from_config() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("RHO_TEST_KEY_RESOLVE".to_owned());
        temp_env::with_var("RHO_TEST_KEY_RESOLVE", Some("secret"), || {
            assert_eq!(resolve_api_key(&config, None), Some("secret".to_owned()));
        });
    }

    #[test]
    fn resolve_api_key_override_beats_config() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("CONFIG_ENV".to_owned());
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
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("RHO_TEST_EMPTY_KEY".to_owned());
        temp_env::with_var("RHO_TEST_EMPTY_KEY", Some(""), || {
            assert!(resolve_api_key(&config, None).is_none());
        });
    }

    // ── is_local_endpoint ──────────────────────────────────────────────────

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
