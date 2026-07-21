//! The [`Provider`] trait, [`OpenAiCompatibleProvider`] implementation,
//! and [`ProviderRegistry`] for multi-provider management.
//!
//! A provider encapsulates identity, externality, model discovery, and
//! access to a [`LlmService`](rho_ai::LlmService). The binary constructs providers via
//! [`ProviderRegistry::from_config`] and uses them for consent checks,
//! model resolution, and obtaining an LLM service. The agent loop,
//! session, and tools only see [`LlmService`](rho_ai::LlmService) — they are unaware of
//! providers.

use crate::client::{ModelInfo, ModelList, RhoAiClient};
use crate::config::{ApiProtocol, ProviderSettings};
use crate::error::Result;
use async_trait::async_trait;
use tracing;

/// A model provider — knows how to authenticate, discover models, and
/// vend an [`LlmService`](rho_ai::LlmService).
///
/// The binary constructs providers via [`ProviderRegistry::from_config`]
/// and uses them for consent checks, model resolution, and obtaining an
/// LLM service. The agent loop, session, and tools only see
/// [`LlmService`](rho_ai::LlmService) — they are unaware of providers.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable provider name (e.g. `"local"`, `"openrouter"`).
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

    /// The LLM service for this provider.
    ///
    /// The returned reference borrows `self`, so the provider must outlive
    /// any request made through the service.
    fn llm_service(&self) -> &dyn rho_ai::LlmService;

    /// Return a clone of the underlying LLM service as a boxed trait object.
    ///
    /// Used by callers that need an owned service (e.g. wrapping in a
    /// `CountingService` for benchmarks).
    fn clone_boxed_service(&self) -> Box<dyn rho_ai::LlmService>;
}

/// An OpenAI-compatible provider.
///
/// Wraps a [`RhoAiClient`] and implements [`Provider`]. Supports any
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
    /// Human-readable provider name.
    name: String,
    /// The underlying chat client.
    client: RhoAiClient,
    /// Whether this provider is external (non-localhost).
    is_external: bool,
}
impl OpenAiCompatibleProvider {
    /// Create a provider with the given name, endpoint, and optional bearer auth.
    ///
    /// Derives `is_external` from the endpoint URL via
    /// [`is_local_endpoint`].
    ///
    /// [`is_local_endpoint`]: crate::client::is_local_endpoint
    pub fn new(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self::with_protocol(name, endpoint, api_key, ApiProtocol::ChatCompletions)
    }

    /// Create a provider with an explicit request protocol.
    pub fn with_protocol(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<String>,
        protocol: ApiProtocol,
    ) -> Self {
        let endpoint_str = endpoint.into();
        let is_external = !crate::client::is_local_endpoint(&endpoint_str);
        let client = RhoAiClient::with_protocol(&endpoint_str, api_key, protocol);
        Self {
            name: name.into(),
            client,
            is_external,
        }
    }

    /// Create a provider with an explicit models endpoint.
    ///
    /// `models_endpoint` overrides the URL used for model discovery. When
    /// `None`, the models URL is derived from the chat endpoint.
    pub fn with_models_endpoint(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<String>,
        models_endpoint: Option<String>,
    ) -> Self {
        Self::with_models_endpoint_and_protocol(
            name,
            endpoint,
            api_key,
            models_endpoint,
            ApiProtocol::ChatCompletions,
        )
    }

    /// Create a provider with explicit model discovery and request protocol.
    pub fn with_models_endpoint_and_protocol(
        name: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<String>,
        models_endpoint: Option<String>,
        protocol: ApiProtocol,
    ) -> Self {
        let endpoint_str = endpoint.into();
        let is_external = !crate::client::is_local_endpoint(&endpoint_str);
        let client = RhoAiClient::with_models_endpoint_and_protocol(
            &endpoint_str,
            api_key,
            models_endpoint,
            protocol,
        );
        Self {
            name: name.into(),
            client,
            is_external,
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_external(&self) -> bool {
        self.is_external
    }

    async fn list_models(&self) -> Result<crate::client::ModelList> {
        self.client.list_models().await
    }

    fn llm_service(&self) -> &dyn rho_ai::LlmService {
        // RhoAiClient creates the configured rho-ai transport per call.
        &self.client
    }

    fn clone_boxed_service(&self) -> Box<dyn rho_ai::LlmService> {
        Box::new(self.client.clone())
    }
}

/// The default endpoint URL when no override or config is set.
const DEFAULT_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";

/// Summary of a configured provider for display purposes.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProviderInfo {
    /// Provider name.
    pub name: String,
    /// Whether this provider sends data externally.
    pub is_external: bool,
    /// Whether the provider's `/v1/models` endpoint is reachable.
    pub reachable: bool,
}

/// A named collection of model providers.
///
/// Owns one or more `Box<dyn Provider>` instances. Provides lookup by name,
/// cross-provider model discovery, and default provider selection.
///
/// Constructed via [`ProviderRegistry::from_config`] from [`ProviderSettings`].
pub struct ProviderRegistry {
    /// Ordered list of boxed provider trait objects.
    providers: Vec<Box<dyn Provider>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    /// Create an empty registry.
    ///
    /// Useful for tests that need to populate providers manually.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Add a provider to the registry.
    pub fn add(&mut self, provider: Box<dyn Provider>) {
        self.providers.push(provider);
    }

    /// Construct a registry from config with optional CLI overrides.
    ///
    /// CLI `--endpoint` and `--api-key-env` override the **default (first)**
    /// provider's endpoint and API key. Additional providers use their
    /// configured values.
    ///
    /// If no providers are configured (empty settings), a default localhost
    /// provider is appended to preserve the zero-config UX.
    pub fn from_config(
        settings: &ProviderSettings,
        endpoint_override: Option<&str>,
        api_key_env_override: Option<&str>,
    ) -> Self {
        let providers: Vec<Box<dyn Provider>> = if settings.providers.is_empty() {
            // Apply CLI overrides even in the zero-config case so that
            // `--endpoint` and `--api-key-env` work without a config file.
            let endpoint =
                endpoint_override.map_or_else(|| DEFAULT_ENDPOINT.to_owned(), String::from);
            let api_key = api_key_env_override.and_then(|var| std::env::var(var).ok());
            vec![Box::new(OpenAiCompatibleProvider::new(
                "local", endpoint, api_key,
            ))]
        } else {
            settings
                .providers
                .iter()
                .enumerate()
                .map(|(i, config)| {
                    let name: &str = config.name.as_deref().unwrap_or_else(|| {
                        config.r#type.as_deref().unwrap_or_else(|| {
                            config
                                .endpoint
                                .as_deref()
                                .and_then(|ep| {
                                    reqwest::Url::parse(ep).ok().and_then(|u| {
                                        u.host_str().map(|h| h.to_owned().leak() as &str)
                                    })
                                })
                                .unwrap_or_else(|| i.to_string().leak())
                        })
                    });

                    let endpoint = if i == 0 {
                        endpoint_override
                            .map(String::from)
                            .or_else(|| config.endpoint.clone())
                    } else {
                        config.endpoint.clone()
                    };
                    let endpoint = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

                    let api_key_env = if i == 0 {
                        api_key_env_override.or(config.api_key_env.as_deref())
                    } else {
                        config.api_key_env.as_deref()
                    };
                    let api_key = api_key_env.and_then(|var| std::env::var(var).ok());
                    let models_endpoint = config.models_endpoint.clone();
                    let protocol = config.api.unwrap_or_default();

                    Box::new(OpenAiCompatibleProvider::with_models_endpoint_and_protocol(
                        name,
                        endpoint,
                        api_key,
                        models_endpoint,
                        protocol,
                    )) as Box<dyn Provider>
                })
                .collect()
        };

        Self { providers }
    }

    /// All registered providers.
    #[must_use]
    pub fn providers(&self) -> &[Box<dyn Provider>] {
        &self.providers
    }

    /// Find a provider by name (exact match).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Provider> {
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(std::convert::AsRef::as_ref)
    }

    /// Find the index of a provider by name (exact match).
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.providers.iter().position(|p| p.name() == name)
    }

    /// The default (first) provider.
    ///
    /// # Panics
    ///
    /// Panics if the registry is empty. Callers should check `is_empty()`
    /// or handle the error from `from_config`.
    #[must_use]
    pub fn default(&self) -> &dyn Provider {
        self.providers[0].as_ref()
    }

    /// Whether the registry has any providers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Collect all models across all providers.
    ///
    /// Queries each provider's `list_models()` endpoint. Returns a vec of
    /// `(provider_name, ModelInfo)` pairs for disambiguation.
    ///
    /// Providers that are unreachable are silently skipped (with a warning
    /// logged). Returns an empty vec if all providers fail.
    pub async fn list_all_models(&self) -> Vec<(&str, ModelInfo)> {
        // Fan out all providers concurrently so a slow or unreachable
        // provider does not block model discovery on faster ones.
        use futures::future::join_all;

        let results: Vec<(usize, Result<ModelList>)> = join_all(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| async move { (i, p.list_models().await) }),
        )
        .await;

        let mut result = Vec::new();
        for (i, res) in results {
            match res {
                Ok(list) => {
                    for model in list.into_data() {
                        result.push((self.providers[i].name(), model));
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        provider = self.providers[i].name(),
                        "failed to list models: {e}"
                    );
                }
            }
        }
        result
    }

    /// Find which provider has a given model ID.
    ///
    /// Searches providers in order. Returns the first provider that has
    /// a model matching the given ID.
    pub async fn find_model(&self, model_id: &str) -> Option<(&dyn Provider, ModelInfo)> {
        // Query all providers concurrently, then pick the first (in
        // registry order) that has the model.
        use futures::future::join_all;

        let results: Vec<(usize, Result<ModelList>)> = join_all(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| async move { (i, p.list_models().await) }),
        )
        .await;

        for (i, res) in results {
            if let Ok(list) = res
                && let Some(model) = list.into_data().into_iter().find(|m| m.id() == model_id)
            {
                return Some((self.providers[i].as_ref(), model));
            }
        }
        None
    }

    /// Find which provider has a given model ID, returning its index.
    ///
    /// Searches providers in order. Returns the index of the first
    /// provider whose `list_models()` includes a model matching `model_id`,
    /// or `None` if no provider has it.
    pub async fn find_model_index(&self, model_id: &str) -> Option<usize> {
        // Query all providers concurrently, then pick the first (in
        // registry order) that has the model.
        use futures::future::join_all;

        let results: Vec<(usize, Result<ModelList>)> = join_all(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| async move { (i, p.list_models().await) }),
        )
        .await;

        for (i, res) in results {
            if let Ok(list) = res
                && list.data().iter().any(|m| m.id() == model_id)
            {
                return Some(i);
            }
        }
        None
    }

    /// Collect the names of all external providers.
    pub fn external_provider_names(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|p| p.is_external())
            .map(|p| p.name())
            .collect()
    }

    /// List all configured providers with reachability status.
    ///
    /// Probes each provider's `/v1/models` endpoint to determine
    /// reachability. Returns [`ProviderInfo`] structs suitable for
    /// display (e.g. the `/providers` REPL command).
    pub async fn list_providers(&self) -> Vec<ProviderInfo> {
        // Probe all providers concurrently so a slow provider doesn't
        // block the reachability check on faster ones.
        use futures::future::join_all;

        let results: Vec<(usize, Result<ModelList>)> = join_all(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| async move { (i, p.list_models().await) }),
        )
        .await;

        let mut result = Vec::new();
        for (i, res) in results {
            let name = self.providers[i].name();
            match res {
                Ok(_) => {
                    tracing::debug!(provider = %name, "provider reachable");
                    result.push(ProviderInfo {
                        name: name.to_owned(),
                        is_external: self.providers[i].is_external(),
                        reachable: true,
                    });
                }
                Err(e) => {
                    tracing::warn!(provider = %name, error = %e, "provider unreachable");
                    result.push(ProviderInfo {
                        name: name.to_owned(),
                        is_external: self.providers[i].is_external(),
                        reachable: false,
                    });
                }
            }
        }
        result
    }
}

/// Construct a fully-configured [`OpenAiCompatibleProvider`] from
/// [`crate::config::RhoConfig`].
///
/// **Deprecated:** Use [`ProviderRegistry::from_config`] for new code.
/// This function is preserved for backward compatibility with
/// `rho-bench` and test code.
///
/// # Priority
///
/// - **Endpoint:** CLI override → config `provider.endpoint` → localhost default.
/// - **API key:** CLI override → config `provider.api_key_env`.
pub fn provider_factory(
    config: &crate::config::RhoConfig,
    endpoint_override: Option<&str>,
    api_key_env_override: Option<&str>,
) -> Box<dyn Provider> {
    let endpoint = endpoint_override
        .map(String::from)
        .or_else(|| config.provider.default_endpoint().map(String::from))
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());

    let api_key = crate::client::resolve_api_key(config, api_key_env_override);

    let name = config
        .provider
        .default_provider()
        .and_then(|p| p.name.as_deref())
        .unwrap_or("local");

    let protocol = config
        .provider
        .default_provider()
        .and_then(|provider| provider.api)
        .unwrap_or_default();

    Box::new(OpenAiCompatibleProvider::with_protocol(
        name, endpoint, api_key, protocol,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::is_local_endpoint;
    use crate::config::{ProviderConfig, RhoConfig};

    // ── OpenAiCompatibleProvider ──────────────────────────────────────────────

    #[test]
    fn provider_new_local() {
        let p = OpenAiCompatibleProvider::new(
            "local",
            "http://localhost:1234/v1/chat/completions",
            None,
        );
        assert_eq!(p.name(), "local");
        assert!(!p.is_external());
        assert_eq!(p.client.protocol(), ApiProtocol::ChatCompletions);
    }

    #[test]
    fn provider_protocol_constructor_selects_responses() {
        let p = OpenAiCompatibleProvider::with_protocol(
            "openai",
            "https://api.openai.com/v1/responses",
            Some("sk-test".to_owned()),
            ApiProtocol::Responses,
        );
        assert_eq!(p.client.protocol(), ApiProtocol::Responses);
    }

    #[test]
    fn provider_new_external() {
        let p = OpenAiCompatibleProvider::new(
            "openai",
            "https://api.openai.com/v1/chat/completions",
            Some("sk-test".to_owned()),
        );
        assert_eq!(p.name(), "openai");
        assert!(p.is_external());
    }

    #[test]
    fn provider_new_ipv6_loopback_is_local() {
        let p =
            OpenAiCompatibleProvider::new("local", "http://[::1]:1234/v1/chat/completions", None);
        assert!(!p.is_external());
    }

    #[test]
    fn provider_new_ipv6_loopback_bracketed_is_local() {
        assert!(is_local_endpoint("http://[::1]:1234/v1/chat/completions"));
    }

    #[test]
    fn provider_clone_boxed_service() {
        let p = OpenAiCompatibleProvider::new(
            "local",
            "http://localhost:1234/v1/chat/completions",
            Some("key".to_owned()),
        );
        let _cloned = p.clone_boxed_service();
        // The clone is a Box<dyn LlmService> — verified it doesn't panic.
    }

    // ── ProviderRegistry ─────────────────────────────────────────────────────

    #[test]
    fn registry_from_empty_settings() {
        let settings = ProviderSettings::default();
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.default().name(), "local");
        assert!(!registry.default().is_external());
    }

    #[test]
    fn registry_single_provider() {
        let settings = ProviderSettings {
            providers: vec![ProviderConfig {
                name: Some("openai".to_owned()),
                endpoint: Some("https://api.openai.com/v1/chat/completions".to_owned()),
                api_key_env: Some("OPENAI_API_KEY".to_owned()),
                ..Default::default()
            }],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.default().name(), "openai");
        assert!(registry.default().is_external());
    }

    #[test]
    fn registry_multiple_providers() {
        let settings = ProviderSettings {
            providers: vec![
                ProviderConfig {
                    name: Some("local".to_owned()),
                    endpoint: Some("http://localhost:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
                ProviderConfig {
                    name: Some("openrouter".to_owned()),
                    endpoint: Some("https://openrouter.ai/api/v1/chat/completions".to_owned()),
                    api_key_env: Some("OR_KEY".to_owned()),
                    ..Default::default()
                },
            ],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.default().name(), "local");

        assert!(registry.get("local").is_some());
        assert!(registry.get("openrouter").is_some());
        assert!(registry.get("nonexistent").is_none());

        let or = registry.get("openrouter").unwrap();
        assert!(or.is_external());
    }

    #[test]
    fn registry_endpoint_override_applies_to_default_only() {
        let settings = ProviderSettings {
            providers: vec![
                ProviderConfig {
                    name: Some("primary".to_owned()),
                    endpoint: Some("http://primary:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
                ProviderConfig {
                    name: Some("secondary".to_owned()),
                    endpoint: Some("http://secondary:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
            ],
        };
        let registry = ProviderRegistry::from_config(
            &settings,
            Some("http://override:9999/v1/chat/completions"),
            None,
        );

        // Override applies to default (first) provider.
        let default = registry.default();
        assert!(default.is_external()); // override host is not localhost
        assert_eq!(default.name(), "primary");

        // Second provider keeps its configured endpoint.
        let secondary = registry.get("secondary").unwrap();
        assert_eq!(secondary.name(), "secondary");
    }

    #[test]
    fn registry_api_key_override_applies_to_default_only() {
        temp_env::with_vars(
            [
                ("OVERRIDE_KEY", Some("override-val")),
                ("SECONDARY_KEY", Some("secondary-val")),
            ],
            || {
                let settings = ProviderSettings {
                    providers: vec![
                        ProviderConfig {
                            name: Some("primary".to_owned()),
                            endpoint: Some("http://primary:1234/v1/chat/completions".to_owned()),
                            api_key_env: Some("PRIMARY_KEY".to_owned()),
                            ..Default::default()
                        },
                        ProviderConfig {
                            name: Some("secondary".to_owned()),
                            endpoint: Some("http://secondary:1234/v1/chat/completions".to_owned()),
                            api_key_env: Some("SECONDARY_KEY".to_owned()),
                            ..Default::default()
                        },
                    ],
                };
                let registry = ProviderRegistry::from_config(&settings, None, Some("OVERRIDE_KEY"));

                // Primary uses override key.
                let primary = registry.default();
                let client = primary.clone_boxed_service();

                // Secondary uses its own configured key.
                let secondary = registry.get("secondary").unwrap();
                let _sec_client = secondary.clone_boxed_service();

                // We can't inspect the key directly, but we verified
                // construction didn't panic and both services were created.
                let _ = client;
            },
        );
    }

    #[test]
    fn registry_index_of() {
        let settings = ProviderSettings {
            providers: vec![
                ProviderConfig {
                    name: Some("a".to_owned()),
                    ..Default::default()
                },
                ProviderConfig {
                    name: Some("b".to_owned()),
                    ..Default::default()
                },
            ],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.index_of("a"), Some(0));
        assert_eq!(registry.index_of("b"), Some(1));
        assert_eq!(registry.index_of("c"), None);
    }

    #[test]
    fn registry_external_provider_names() {
        let settings = ProviderSettings {
            providers: vec![
                ProviderConfig {
                    name: Some("local".to_owned()),
                    endpoint: Some("http://localhost:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
                ProviderConfig {
                    name: Some("openrouter".to_owned()),
                    endpoint: Some("https://openrouter.ai/api/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
            ],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        let names = registry.external_provider_names();
        assert_eq!(names, vec!["openrouter"]);
    }

    #[test]
    fn registry_default_name_from_endpoint_host() {
        let settings = ProviderSettings {
            providers: vec![
                ProviderConfig {
                    // No name set — derives from endpoint host.
                    endpoint: Some("http://localhost:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
                ProviderConfig {
                    // No name set — derives from endpoint host.
                    endpoint: Some("http://other:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                },
            ],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.default().name(), "localhost");
        assert_eq!(registry.get("other").unwrap().name(), "other");
    }

    #[test]
    fn registry_name_falls_back_to_index_when_no_endpoint() {
        let settings = ProviderSettings {
            providers: vec![ProviderConfig {
                // No name, no type, no endpoint — falls back to index.
                ..Default::default()
            }],
        };
        let registry = ProviderRegistry::from_config(&settings, None, None);
        assert_eq!(registry.default().name(), "0");
    }

    // ── provider_factory ──────────────────────────────────────────────────────

    #[test]
    fn provider_factory_returns_openai_compatible() {
        let config = RhoConfig::default();
        let provider = provider_factory(&config, None, None);
        assert_eq!(provider.name(), "local");
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
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    endpoint: Some("http://config.com/v1/chat/completions".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let provider = provider_factory(
            &config,
            Some("https://api.openai.com/v1/chat/completions"),
            None,
        );
        assert!(provider.is_external());
    }

    #[test]
    fn provider_factory_uses_config_endpoint() {
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    endpoint: Some("https://openrouter.ai/api/v1/chat/completions".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let provider = provider_factory(&config, None, None);
        assert!(provider.is_external());
    }

    #[test]
    fn provider_factory_respects_api_key_config() {
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    api_key_env: Some("RHO_TEST_PROVIDER_KEY".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        temp_env::with_var("RHO_TEST_PROVIDER_KEY", Some("test-key"), || {
            let provider = provider_factory(&config, None, None);
            assert_eq!(provider.name(), "local");
        });
    }

    #[test]
    fn provider_factory_api_key_override_beats_config() {
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    api_key_env: Some("CONFIG_KEY".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        temp_env::with_vars(
            [
                ("CONFIG_KEY", Some("config-key")),
                ("OVERRIDE_KEY", Some("override-key")),
            ],
            || {
                let provider = provider_factory(&config, None, Some("OVERRIDE_KEY"));
                assert_eq!(provider.name(), "local");
            },
        );
    }

    #[test]
    fn provider_factory_uses_config_name() {
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    name: Some("my-provider".to_owned()),
                    endpoint: Some("http://localhost:1234/v1/chat/completions".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let provider = provider_factory(&config, None, None);
        assert_eq!(provider.name(), "my-provider");
    }
    // ── Concurrent model discovery ────────────────────────────────────────
    //
    // The registry must query all providers concurrently so a slow or
    // unreachable provider does not block model discovery on faster ones.
    // These tests use `DelayedTestProvider` to verify that the total wall
    // time is bounded by the slowest single provider, not the sum of all.

    use std::time::{Duration, Instant};

    /// A mock provider that returns a fixed model list, optionally with a delay.
    /// Used to verify that the registry queries providers concurrently.
    struct MockProvider {
        name: String,
        models: Vec<String>,
        delay: Duration,
    }

    impl MockProvider {
        fn new(name: &str, models: &[&str], delay: Duration) -> Self {
            Self {
                name: name.to_owned(),
                models: models
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                delay,
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn is_external(&self) -> bool {
            false
        }

        async fn list_models(&self) -> Result<ModelList> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(ModelList::Standard {
                data: self
                    .models
                    .iter()
                    .map(|id| ModelInfo::Standard {
                        id: id.clone(),
                        object: "model".to_owned(),
                        created: 0,
                        owned_by: "test".to_owned(),
                    })
                    .collect(),
            })
        }

        fn llm_service(&self) -> &dyn rho_ai::LlmService {
            unimplemented!("MockProvider does not support LLM calls")
        }

        fn clone_boxed_service(&self) -> Box<dyn rho_ai::LlmService> {
            unimplemented!("MockProvider does not support LLM calls")
        }
    }
    fn registry_with_delayed_provider() -> ProviderRegistry {
        // Both providers have a 200ms delay. Sequential queries would
        // take ~400ms; concurrent queries take ~200ms. The 300ms threshold
        // distinguishes the two.
        let mut reg = ProviderRegistry::new();
        reg.add(Box::new(MockProvider::new(
            "alpha",
            &["alpha-model"],
            Duration::from_millis(200),
        )));
        reg.add(Box::new(MockProvider::new(
            "beta",
            &["beta-model"],
            Duration::from_millis(200),
        )));
        reg
    }

    #[tokio::test]
    async fn find_model_index_is_concurrent() {
        let reg = registry_with_delayed_provider();
        let start = Instant::now();
        // Both providers have a 200ms delay. Sequential queries would
        // take ~400ms; concurrent queries take ~200ms. The 300ms threshold
        // distinguishes the two.
        let idx = reg.find_model_index("beta-model").await;
        let elapsed = start.elapsed();
        assert_eq!(idx, Some(1));
        assert!(
            elapsed < Duration::from_millis(300),
            "find_model_index should be concurrent: took {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn find_model_index_returns_first_match_in_order() {
        // Both providers serve "shared-model" — the fast one (index 0)
        // must win, not whichever concurrent query finishes first.
        let mut reg = ProviderRegistry::new();
        reg.add(Box::new(MockProvider::new(
            "fast",
            &["shared-model"],
            Duration::ZERO,
        )));
        reg.add(Box::new(MockProvider::new(
            "slow",
            &["shared-model"],
            Duration::from_millis(200),
        )));
        let idx = reg.find_model_index("shared-model").await;
        assert_eq!(idx, Some(0));
    }

    #[tokio::test]
    async fn list_all_models_is_concurrent() {
        let reg = registry_with_delayed_provider();
        let start = Instant::now();
        let models = reg.list_all_models().await;
        let elapsed = start.elapsed();
        // Should have models from both providers.
        assert_eq!(models.len(), 2);
        assert!(
            elapsed < Duration::from_millis(300),
            "list_all_models should be concurrent: took {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn list_providers_is_concurrent() {
        let reg = registry_with_delayed_provider();
        let start = Instant::now();
        let providers = reg.list_providers().await;
        let elapsed = start.elapsed();
        // Both providers should be reachable.
        assert_eq!(providers.len(), 2);
        assert!(providers.iter().all(|p| p.reachable));
        assert!(
            elapsed < Duration::from_millis(300),
            "list_providers should be concurrent: took {elapsed:?}",
        );
    }
}
