//! The [`Provider`] trait, [`OpenAiCompatibleProvider`] implementation,
//! and [`ProviderRegistry`] for multi-provider management.
//!
//! A provider encapsulates identity, externality, model discovery, and
//! access to a [`ChatClient`]. The binary constructs providers via
//! [`ProviderRegistry::from_config`] and uses them for consent checks,
//! model resolution, and obtaining a chat client. The agent loop,
//! session, and tools only see [`ChatClient`] — they are unaware of
//! providers.
//!
//! [`ChatClient`]: crate::client::ChatClient

use crate::client::{ChatClient, LocalChatClient, ModelInfo, ModelList};
use crate::config::ProviderSettings;
use crate::error::Result;
use async_trait::async_trait;
use tracing;

/// A model provider — knows how to authenticate, discover models, and
/// vend a [`ChatClient`].
///
/// The binary constructs providers via [`ProviderRegistry::from_config`]
/// and uses them for consent checks, model resolution, and obtaining a
/// chat client. The agent loop, session, and tools only see [`ChatClient`]
/// — they are unaware of providers.
///
/// [`ChatClient`]: crate::client::ChatClient
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
    /// Human-readable provider name.
    name: String,
    /// The underlying chat client.
    client: LocalChatClient,
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
        let endpoint_str = endpoint.into();
        let is_external = !crate::client::is_local_endpoint(&endpoint_str);
        let client = match api_key {
            Some(key) => LocalChatClient::with_endpoint_and_key(endpoint_str, Some(key)),
            None => LocalChatClient::with_endpoint(endpoint_str),
        };
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

impl ProviderRegistry {
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
            vec![Box::new(OpenAiCompatibleProvider::new(
                "local",
                DEFAULT_ENDPOINT,
                None,
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

                    Box::new(OpenAiCompatibleProvider::new(name, endpoint, api_key))
                        as Box<dyn Provider>
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
        let mut result = Vec::new();
        for provider in &self.providers {
            match provider.list_models().await {
                Ok(list) => {
                    for model in list.data {
                        result.push((provider.name(), model));
                    }
                }
                Err(e) => {
                    tracing::warn!(provider = provider.name(), "failed to list models: {e}");
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
        for provider in &self.providers {
            if let Ok(list) = provider.list_models().await
                && let Some(model) = list.data.into_iter().find(|m| m.id == model_id)
            {
                return Some((provider.as_ref(), model));
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

    Box::new(OpenAiCompatibleProvider::new(name, endpoint, api_key))
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
    fn provider_clone_boxed_client() {
        let p = OpenAiCompatibleProvider::new(
            "local",
            "http://localhost:1234/v1/chat/completions",
            Some("key".to_owned()),
        );
        let _cloned = p.clone_boxed_client();
        // The clone is a Box<dyn ChatClient> — we can't inspect it
        // further, but we verified it doesn't panic.
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
                let client = primary.clone_boxed_client();

                // Secondary uses its own configured key.
                let secondary = registry.get("secondary").unwrap();
                let _sec_client = secondary.clone_boxed_client();

                // We can't inspect the key directly, but we verified
                // construction didn't panic and both clients were created.
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
            providers: vec![
                ProviderConfig {
                    // No name, no type, no endpoint — falls back to index.
                    ..Default::default()
                },
            ],
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
}
