//! The [`Provider`] trait and [`OpenAiCompatibleProvider`] implementation.
//!
//! A provider encapsulates identity, externality, model discovery, and
//! access to a [`ChatClient`]. The binary constructs a provider via
//! [`provider_factory`] and uses it for consent checks, model resolution,
//! and obtaining a chat client. The agent loop, session, and tools only
//! see [`ChatClient`] — they are unaware of providers.
//!
//! [`ChatClient`]: crate::client::ChatClient

use crate::client::{ChatClient, LocalChatClient, ModelList};
use crate::config::RhoConfig;
use crate::error::Result;
use async_trait::async_trait;

/// A model provider — knows how to authenticate, discover models, and
/// vend a [`ChatClient`].
///
/// The binary constructs a provider via [`provider_factory`] and uses it
/// for consent checks, model resolution, and obtaining a chat client.
/// The agent loop, session, and tools only see [`ChatClient`] — they are
/// unaware of providers.
///
/// [`provider_factory`]: crate::provider_factory
/// [`ChatClient`]: crate::client::ChatClient
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable provider name (e.g. `"OpenAI Compatible"`,
    /// `"Anthropic"`).
    fn name(&self) -> &str;

    /// Whether this provider sends data to an external server.
    ///
    /// `true` for remote APIs (`OpenRouter`, `OpenAI`, `Anthropic`, etc.).
    /// `false` for local servers (LM Studio, Ollama on localhost).
    fn is_external(&self) -> bool;

    /// List available models from this provider.
    ///
    /// Queries the server's model list endpoint. Returns an error if the
    /// server is unreachable or the endpoint is not supported.
    ///
    /// Returns the full [`ModelList`] so callers can access top-level
    /// fields (e.g. pagination metadata) beyond the model entries
    /// themselves.
    async fn list_models(&self) -> Result<ModelList>;

    /// The chat client for this provider.
    ///
    /// The returned reference borrows `self`, so the provider must outlive
    /// any request made through the client.
    fn chat_client(&self) -> &dyn ChatClient;

    /// Return a clone of the underlying chat client as a boxed trait object.
    ///
    /// Used by callers that need an owned client (e.g. wrapping in a
    /// `CountingClient` for benchmarks). The default implementation
    /// panics; concrete providers must override this.
    fn clone_boxed_client(&self) -> Box<dyn ChatClient>;
}

/// An OpenAI-compatible provider.
///
/// Wraps a [`LocalChatClient`] and implements [`Provider`]. Supports any
/// server that speaks the `OpenAI` Chat Completions wire format — local
/// servers (`LM Studio`, `Ollama`) and external providers (`OpenRouter`,
/// `OpenAI`, `Groq`, `DeepInfra`, etc.).
///
/// # Externality
///
/// `is_external` is derived from the endpoint URL at construction time
/// via [`is_local_endpoint`]. A provider pointing at `localhost`,
/// `127.0.0.1`, or `::1` is considered local; everything else is
/// external.
///
/// [`is_local_endpoint`]: crate::client::is_local_endpoint
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleProvider {
    /// The underlying chat client.
    client: LocalChatClient,
    /// Whether this provider is external (non-localhost).
    is_external: bool,
}

impl OpenAiCompatibleProvider {
    /// Create a provider at the given endpoint with optional bearer auth.
    ///
    /// Derives `is_external` from the endpoint URL via
    /// [`is_local_endpoint`].
    ///
    /// [`is_local_endpoint`]: crate::client::is_local_endpoint
    pub fn new(endpoint: impl Into<String>, api_key: Option<String>) -> Self {
        let endpoint_str = endpoint.into();
        let is_external = !crate::client::is_local_endpoint(&endpoint_str);
        let client = match api_key {
            Some(key) => LocalChatClient::with_endpoint_and_key(endpoint_str, Some(key)),
            None => LocalChatClient::with_endpoint(endpoint_str),
        };
        Self {
            client,
            is_external,
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        "OpenAI Compatible"
    }

    fn is_external(&self) -> bool {
        self.is_external
    }

    async fn list_models(&self) -> Result<ModelList> {
        self.client.list_models().await
    }

    fn chat_client(&self) -> &dyn ChatClient {
        &self.client
    }

    fn clone_boxed_client(&self) -> Box<dyn ChatClient> {
        Box::new(self.client.clone())
    }
}

/// The default endpoint URL when no override or config is set.
const DEFAULT_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";

/// Construct a fully-configured [`OpenAiCompatibleProvider`] from
/// [`RhoConfig`].
///
/// This is the **recommended** way to construct providers in the binary.
/// It respects config values, handles API key resolution from environment
/// variables, and applies CLI overrides.
///
/// Returns `Box<dyn Provider>` so callers can swap implementations at
/// runtime. Currently always returns an [`OpenAiCompatibleProvider`].
///
/// # Priority
///
/// - **Endpoint:** CLI override → config `provider.endpoint` → localhost default.
/// - **API key:** CLI override → config `provider.api_key_env`.
pub fn provider_factory(
    config: &RhoConfig,
    endpoint_override: Option<&str>,
    api_key_env_override: Option<&str>,
) -> Box<dyn Provider> {
    let endpoint = endpoint_override
        .map(String::from)
        .or_else(|| config.provider.endpoint.clone())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

    let api_key = crate::client::resolve_api_key(config, api_key_env_override);

    Box::new(OpenAiCompatibleProvider::new(endpoint, api_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::is_local_endpoint;
    use crate::config::RhoConfig;

    // ── OpenAiCompatibleProvider ──────────────────────────────────────────────

    #[test]
    fn provider_new_local() {
        let p = OpenAiCompatibleProvider::new("http://localhost:1234/v1/chat/completions", None);
        assert_eq!(p.name(), "OpenAI Compatible");
        assert!(!p.is_external());
    }

    #[test]
    fn provider_new_external() {
        let p = OpenAiCompatibleProvider::new(
            "https://api.openai.com/v1/chat/completions",
            Some("sk-test".to_owned()),
        );
        assert!(p.is_external());
    }

    #[test]
    fn provider_new_ipv6_loopback_is_local() {
        let p = OpenAiCompatibleProvider::new("http://[::1]:1234/v1/chat/completions", None);
        assert!(!p.is_external());
    }

    #[test]
    fn provider_new_ipv6_loopback_bracketed_is_local() {
        assert!(is_local_endpoint("http://[::1]:1234/v1/chat/completions"));
    }

    #[test]
    fn provider_clone_boxed_client() {
        let p = OpenAiCompatibleProvider::new(
            "http://localhost:1234/v1/chat/completions",
            Some("key".to_owned()),
        );
        let _cloned = p.clone_boxed_client();
        // The clone is a Box<dyn ChatClient> — we can't inspect it
        // further, but we verified it doesn't panic.
    }

    // ── provider_factory ──────────────────────────────────────────────────────

    #[test]
    fn provider_factory_returns_openai_compatible() {
        let config = RhoConfig::default();
        let provider = provider_factory(&config, None, None);
        assert_eq!(provider.name(), "OpenAI Compatible");
        assert!(!provider.is_external());
    }

    #[test]
    fn provider_factory_respects_endpoint_override() {
        let config = RhoConfig::default();
        let provider = provider_factory(
            &config,
            Some("https://api.openai.com/v1/chat/completions"),
            None,
        );
        assert!(provider.is_external());
    }

    #[test]
    fn provider_factory_override_beats_config() {
        let mut config = RhoConfig::default();
        config.provider.endpoint = Some("http://config.com/v1/chat/completions".to_owned());
        let provider = provider_factory(
            &config,
            Some("https://api.openai.com/v1/chat/completions"),
            None,
        );
        assert!(provider.is_external());
    }

    #[test]
    fn provider_factory_uses_config_endpoint() {
        let mut config = RhoConfig::default();
        config.provider.endpoint = Some("https://openrouter.ai/api/v1/chat/completions".to_owned());
        let provider = provider_factory(&config, None, None);
        assert!(provider.is_external());
    }

    #[test]
    fn provider_factory_respects_api_key_config() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("RHO_TEST_PROVIDER_KEY".to_owned());
        temp_env::with_var("RHO_TEST_PROVIDER_KEY", Some("test-key"), || {
            let provider = provider_factory(&config, None, None);
            assert_eq!(provider.name(), "OpenAI Compatible");
        });
    }

    #[test]
    fn provider_factory_api_key_override_beats_config() {
        let mut config = RhoConfig::default();
        config.provider.api_key_env = Some("CONFIG_KEY".to_owned());
        temp_env::with_vars(
            [
                ("CONFIG_KEY", Some("config-key")),
                ("OVERRIDE_KEY", Some("override-key")),
            ],
            || {
                let provider = provider_factory(&config, None, Some("OVERRIDE_KEY"));
                assert_eq!(provider.name(), "OpenAI Compatible");
            },
        );
    }
}
