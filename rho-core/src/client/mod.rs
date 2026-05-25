//! The [`ChatClient`] trait and [`RhoAiClient`] adapter.
//!
//! [`RhoAiClient`] wraps [`rho_ai::OpenAiService`] and adapts it to rho-core's
//! internal types. All HTTP communication and SSE parsing is delegated to rho-ai.

pub mod error;

use crate::client::error::ClientError;
use crate::config::RhoConfig;
use crate::error::Result;
use crate::message::ChatMessage;
use crate::newtypes::ToolCallId;
use crate::request::ChatRequest;
use crate::response::{FinishReason, ModelChoice, ModelMessage, ModelResponse, ModelUsage};
use crate::stream::StreamChunk;
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};
use std::pin::Pin;
use tracing::info;

use rho_ai::service::LlmService;

/// The type returned by [`ChatClient::chat_stream`].
pub type ModelResponseStream = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

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
    async fn chat_stream(&self, request: ChatRequest) -> Result<ModelResponseStream> {
        let response = self.chat(request).await?;
        let chunks = StreamChunk::from_response(&response);
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

// ── Message conversion: ChatMessage → LlmMessage ─────────────────────────────

/// Convert rho-core's [`ChatMessage`] into rho-ai's [`rho_ai::LlmMessage`].
fn to_llm_messages(messages: Vec<ChatMessage>) -> Vec<rho_ai::LlmMessage> {
    messages
        .into_iter()
        .map(|m| match m {
            ChatMessage::System { content } => {
                let text = content_into_string(content);
                rho_ai::LlmMessage::System(text)
            }
            ChatMessage::User { content } => {
                let text = content_into_string(content);
                rho_ai::LlmMessage::User(text)
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let text = if content.is_empty() {
                    None
                } else {
                    Some(content_into_string(content))
                };
                let tc: Vec<rho_ai::ToolCall> = tool_calls
                    .into_iter()
                    .map(|tc| rho_ai::ToolCall {
                        id: tc.id.to_string(),
                        name: tc.function.name.to_string(),
                        arguments: tc.function.arguments,
                    })
                    .collect();
                rho_ai::LlmMessage::Assistant {
                    content: text,
                    tool_calls: tc,
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                let text = content_into_string(content);
                rho_ai::LlmMessage::Tool {
                    tool_call_id: tool_call_id.to_string(),
                    content: text,
                }
            }
        })
        .collect()
}

/// Convert rho-core's [`ToolSchema`](crate::schema::ToolSchema) into rho-ai's
/// [`rho_ai::ToolDefinition`].
fn to_llm_tools(tools: Vec<crate::schema::ToolSchema>) -> Vec<rho_ai::ToolDefinition> {
    tools
        .into_iter()
        .map(|t| {
            rho_ai::ToolDefinition::new(
                t.function.name,
                t.function.description,
                t.function.parameters,
            )
        })
        .collect()
}

/// Extract plain text from a `Vec<ContentBlock>`.
fn content_into_string(blocks: Vec<crate::message::ContentBlock>) -> String {
    blocks
        .into_iter()
        .map(|b| match b {
            crate::message::ContentBlock::Text { text } => text,
        })
        .reduce(|mut acc, s| {
            acc.push_str(&s);
            acc
        })
        .unwrap_or_default()
}

// ── StreamEvent → StreamChunk conversion ─────────────────────────────────────

/// Convert a rho-ai `StreamEvent` stream into rho-core's `StreamChunk` stream.
///
/// Accumulates tool call events and emits `StreamChunk::ToolCallDelta` for
/// compatibility with the existing `StreamChunk::accumulate` pipeline.
fn adapt_event_stream(
    events: rho_ai::EventStream,
) -> impl Stream<Item = Result<StreamChunk>> + Send {
    #[derive(Default)]
    struct ToolAcc {
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    }

    let mut tool_accs: Vec<ToolAcc> = Vec::new();

    events.filter_map(move |event_result| {
        let event = match event_result {
            Ok(e) => e,
            Err(e) => {
                return std::future::ready(Some(Err(crate::error::RhoError::Client(
                    ClientError::from(e),
                ))));
            }
        };

        let chunk = match event {
            rho_ai::StreamEvent::Text(text) => Some(StreamChunk::TextDelta(text)),
            rho_ai::StreamEvent::Reasoning(text) => Some(StreamChunk::ReasoningDelta(text)),

            rho_ai::StreamEvent::ToolUseStart { index, id, name } => {
                if tool_accs.len() <= index {
                    tool_accs.resize_with(index + 1, ToolAcc::default);
                }
                tool_accs[index].id = Some(id.clone());
                tool_accs[index].name = Some(name.clone());
                Some(StreamChunk::ToolCallDelta {
                    index,
                    id: Some(id),
                    function_name: Some(name),
                    arguments_delta: None,
                })
            }

            rho_ai::StreamEvent::ToolUseInputDelta { index, delta } => {
                if tool_accs.len() <= index {
                    tool_accs.resize_with(index + 1, ToolAcc::default);
                }
                tool_accs[index].arguments.push_str(&delta);
                Some(StreamChunk::ToolCallDelta {
                    index,
                    id: None,
                    function_name: None,
                    arguments_delta: Some(delta),
                })
            }

            rho_ai::StreamEvent::ToolUseComplete { index, tool_call } => {
                // Ensure the accumulator is up to date.
                if tool_accs.len() <= index {
                    tool_accs.resize_with(index + 1, ToolAcc::default);
                }
                // Emit a final delta with the complete arguments for accumulate().
                Some(StreamChunk::ToolCallDelta {
                    index,
                    id: None,
                    function_name: None,
                    arguments_delta: Some(tool_call.arguments),
                })
            }

            rho_ai::StreamEvent::Done { reason, usage: _ } => {
                let finish_reason = match reason {
                    rho_ai::StopReason::EndTurn => FinishReason::Stop,
                    rho_ai::StopReason::ToolUse => FinishReason::ToolCalls,
                    rho_ai::StopReason::Length => FinishReason::Length,
                    rho_ai::StopReason::ContentFilter => FinishReason::ContentFilter,
                    rho_ai::StopReason::Other(s) => FinishReason::Other(s),
                };
                Some(StreamChunk::Done(finish_reason))
            }
        };

        std::future::ready(chunk.map(Ok))
    })
}

// ── Accumulate StreamEvents into a ModelResponse (for non-streaming chat) ─────

/// Accumulate a stream of `StreamEvent`s into a `ModelResponse`.
#[allow(clippy::too_many_lines)]
async fn accumulate_response(events: rho_ai::EventStream, model: &str) -> Result<ModelResponse> {
    use rho_ai::StreamEvent;

    // Track tool calls being accumulated.
    struct ToolAcc {
        id: String,
        name: String,
        arguments: String,
    }

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    let mut finish_reason = FinishReason::Stop;
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut tool_acc_map: std::collections::HashMap<usize, ToolAcc> =
        std::collections::HashMap::new();

    let mut stream = std::pin::pin!(events);
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(StreamEvent::Text(t)) => text.push_str(&t),
            Ok(StreamEvent::Reasoning(r)) => reasoning.push_str(&r),
            Ok(StreamEvent::ToolUseStart { index, id, name }) => {
                tool_acc_map.insert(
                    index,
                    ToolAcc {
                        id,
                        name,
                        arguments: String::new(),
                    },
                );
            }
            Ok(StreamEvent::ToolUseInputDelta { index, delta }) => {
                if let Some(acc) = tool_acc_map.get_mut(&index) {
                    acc.arguments.push_str(&delta);
                }
            }
            Ok(StreamEvent::ToolUseComplete { tool_call, .. }) => {
                tool_calls.push(tool_call);
            }
            Ok(StreamEvent::Done { reason, usage }) => {
                finish_reason = match reason {
                    rho_ai::StopReason::EndTurn => FinishReason::Stop,
                    rho_ai::StopReason::ToolUse => FinishReason::ToolCalls,
                    rho_ai::StopReason::Length => FinishReason::Length,
                    rho_ai::StopReason::ContentFilter => FinishReason::ContentFilter,
                    rho_ai::StopReason::Other(s) => FinishReason::Other(s),
                };
                input_tokens = usage.input_tokens;
                output_tokens = usage.output_tokens;
            }
            Err(e) => {
                return Err(crate::error::RhoError::Client(ClientError::from(e)));
            }
        }
    }

    // Merge accumulated tool calls (from ToolUseStart/InputDelta) with complete ones.
    // If we got ToolUseComplete events, those are authoritative.
    // If not (some providers may not emit them), use accumulated deltas.
    if tool_calls.is_empty() {
        let mut indices: Vec<_> = tool_acc_map.keys().copied().collect();
        indices.sort_unstable();
        for idx in indices {
            if let Some(acc) = tool_acc_map.remove(&idx) {
                tool_calls.push(rho_ai::ToolCall {
                    id: acc.id,
                    name: acc.name,
                    arguments: acc.arguments,
                });
            }
        }
    }

    // Build ModelToolCalls from rho_ai::ToolCall
    let model_tool_calls: Vec<crate::message::ModelToolCall> = tool_calls
        .into_iter()
        .map(|tc| crate::message::ModelToolCall {
            id: ToolCallId::new(tc.id),
            call_type: "function".to_owned(),
            function: crate::message::ToolCallFunction {
                name: crate::newtypes::ToolName::new(tc.name),
                arguments: tc.arguments,
            },
        })
        .collect();

    Ok(ModelResponse {
        id: String::new(),
        object: "chat.completion".to_owned(),
        created: 0,
        model: model.to_owned(),
        choices: vec![ModelChoice {
            index: 0,
            message: ModelMessage {
                content: text,
                reasoning_content: reasoning,
                tool_calls: model_tool_calls,
            },
            logprobs: None,
            finish_reason,
        }],
        usage: ModelUsage {
            prompt_tokens: usize::try_from(input_tokens).unwrap_or(0),
            completion_tokens: usize::try_from(output_tokens).unwrap_or(0),
            total_tokens: usize::try_from(input_tokens + output_tokens).unwrap_or(0),
            completion_tokens_details: None,
        },
        stats: crate::response::ModelStats::default(),
        system_fingerprint: String::new(),
    })
}

// ── RhoAiClient ──────────────────────────────────────────────────────────────

/// A [`ChatClient`] backed by [`rho_ai::OpenAiService`].
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

#[async_trait]
impl ChatClient for RhoAiClient {
    #[tracing::instrument(skip_all, fields(model = %request.model, message_count = request.messages.len(), tool_count = request.tools.len()))]
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse> {
        let llm_messages = to_llm_messages(request.messages);
        let llm_tools = to_llm_tools(request.tools);

        let service = self.service();

        let llm_request = rho_ai::types::LlmRequest {
            model: request.model.clone(),
            messages: llm_messages,
            tools: llm_tools,
            max_tokens: request.max_tokens,
        };

        let event_stream = service.chat_stream(llm_request).await;

        let event_stream =
            event_stream.map_err(|e| crate::error::RhoError::Client(ClientError::from(e)))?;

        let response = accumulate_response(event_stream, &request.model).await?;

        if let Some(choice) = response.choices.first() {
            info!(
                finish_reason = ?choice.finish_reason,
                prompt_tokens = response.usage.prompt_tokens,
                completion_tokens = response.usage.completion_tokens,
                total_tokens = response.usage.total_tokens,
            );
        }

        Ok(response)
    }

    async fn chat_stream(&self, request: ChatRequest) -> Result<ModelResponseStream> {
        info!("sending streaming request to {}", self.endpoint);

        let llm_messages = to_llm_messages(request.messages);
        let llm_tools = to_llm_tools(request.tools);

        let service = self.service();

        let llm_request = rho_ai::types::LlmRequest {
            model: request.model.clone(),
            messages: llm_messages,
            tools: llm_tools,
            max_tokens: request.max_tokens,
        };

        let event_stream = service.chat_stream(llm_request).await;

        let event_stream =
            event_stream.map_err(|e| crate::error::RhoError::Client(ClientError::from(e)))?;

        let adapted = adapt_event_stream(event_stream);
        Ok(Box::pin(adapted))
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

    #[test]
    fn to_llm_messages_system() {
        let msgs = vec![ChatMessage::system_text("You are helpful.")];
        let llm = to_llm_messages(msgs);
        assert_eq!(llm.len(), 1);
        assert!(matches!(&llm[0], rho_ai::LlmMessage::System(t) if t == "You are helpful."));
    }

    #[test]
    fn to_llm_messages_user() {
        let msgs = vec![ChatMessage::user_text("Hello")];
        let llm = to_llm_messages(msgs);
        assert!(matches!(&llm[0], rho_ai::LlmMessage::User(t) if t == "Hello"));
    }

    #[test]
    fn to_llm_messages_assistant_with_tool_calls() {
        let msgs = vec![ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![crate::message::ModelToolCall {
                id: ToolCallId::new("call_1"),
                call_type: "function".to_owned(),
                function: crate::message::ToolCallFunction {
                    name: crate::newtypes::ToolName::new("read_file"),
                    arguments: r#"{"path":"foo.rs"}"#.to_owned(),
                },
            }],
        }];
        let llm = to_llm_messages(msgs);
        match &llm[0] {
            rho_ai::LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                assert!(content.is_none());
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].name, "read_file");
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn to_llm_messages_tool_result() {
        let msgs = vec![ChatMessage::tool_result(ToolCallId::new("c1"), "ok")];
        let llm = to_llm_messages(msgs);
        match &llm[0] {
            rho_ai::LlmMessage::Tool {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "c1");
                assert_eq!(content, "ok");
            }
            _ => panic!("expected Tool"),
        }
    }

    // ── Tool conversion ──────────────────────────────────────────────────

    #[test]
    fn to_llm_tools_converts_schemas() {
        let tools = vec![crate::schema::ToolSchema::function(
            "read_file",
            "Read a file",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let llm = to_llm_tools(tools);
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].name, "read_file");
        assert_eq!(llm[0].description, "Read a file");
    }

    // ── content_into_string ──────────────────────────────────────────────

    #[test]
    fn content_into_string_single_block() {
        let blocks = vec![crate::message::ContentBlock::Text {
            text: "hello".to_owned(),
        }];
        assert_eq!(content_into_string(blocks), "hello");
    }

    #[test]
    fn content_into_string_multiple_blocks() {
        let blocks = vec![
            crate::message::ContentBlock::Text {
                text: "hello ".to_owned(),
            },
            crate::message::ContentBlock::Text {
                text: "world".to_owned(),
            },
        ];
        assert_eq!(content_into_string(blocks), "hello world");
    }
}
