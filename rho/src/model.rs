//! Model resolution.
//!
//! Resolves which model to use based on CLI flags, config, and the built-in
//! catalog. No network calls at startup — discovery (`/v1/models`) is reserved
//! for runtime commands (`/models`, `/model` browsing).
//!
//! After resolving the model string, looks it up in the built-in
//! catalog to enrich it with context window, max tokens, cost, and
//! thinking support. Unknown models (custom/local) fall back to
//! config defaults.

use crate::presenter::RpcPresenter as P;
use anyhow::Result;
use rho_ai::catalog::{Catalog, Model};
use rho_core::RhoConfig;

/// Resolve the model identifier without network calls.
///
/// Priority (first match wins):
///
/// 1. CLI `--model` — use verbatim
/// 2. Config `agent.model` — use verbatim
/// 3. Config `agent.provider` + that provider's `default_model` — use it
/// 4. First provider's `default_model` — use it
/// 5. `RHO_MODEL` environment variable
/// 6. Built-in default (`anthropic/claude-sonnet-4`)
///
/// Model names are accepted as-is. Typos and misconfiguration surface
/// as clear HTTP errors at request time, not as startup blocking calls.
/// This matches the pi pattern: static declaration, request-time validation.
///
/// Enriched model information from the catalog.
///
/// `catalog_model` is `Some` when the model was found in the built-in
/// catalog (i.e. it's a known OpenRouter model). For custom/local models
/// it's `None`, and the caller should fall back to config defaults.
#[derive(Debug)]
pub(crate) struct ResolvedModel {
    /// The raw model identifier string.
    pub id: String,
    /// Which configured provider will handle this model (if known).
    pub provider: Option<String>,
    /// Catalog entry, if the model is known.
    pub catalog_model: Option<Model>,
}

/// Emit catalog enrichment info to the user (stderr) when available.
fn emit_catalog_info(catalog_model: &Option<Model>) {
    if let Some(m) = catalog_model {
        P::model_catalog_info(m.context_window, m.max_tokens, m.thinking.supported);
        tracing::info!(
            model = %m.id,
            context_window = m.context_window,
            max_tokens = m.max_tokens,
            thinking = m.thinking.supported,
            "catalog enrichment applied"
        );
    } else {
        tracing::info!("model not found in catalog; using config defaults");
    }
}

pub(crate) fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
) -> Result<ResolvedModel> {
    // 1. CLI flag takes highest priority.
    if let Some(model) = cli_model {
        P::model_from_source(model, "--model", "cli");
        let catalog_model = Catalog::new().find(model).cloned();
        emit_catalog_info(&catalog_model);
        return Ok(ResolvedModel {
            id: model.clone(),
            provider: None,
            catalog_model,
        });
    }

    // 2. Config agent.model.
    if let Some(model) = config.agent.model.as_deref() {
        let provider = resolve_provider_for_model(model, config);
        P::model_from_source(model, "config", provider.as_deref().unwrap_or("default"));
        let catalog_model = Catalog::new().find(model).cloned();
        emit_catalog_info(&catalog_model);
        return Ok(ResolvedModel {
            id: model.to_owned(),
            provider,
            catalog_model,
        });
    }

    // 3. Config agent.provider + that provider's default_model.
    if let Some(provider_name) = config.agent.provider.as_deref() {
        if let Some(provider_config) = config
            .provider
            .providers
            .iter()
            .find(|p| p.name.as_deref() == Some(provider_name))
        {
            if let Some(model) = provider_config.default_model.as_deref() {
                P::model_from_source(model, "config", provider_name);
                let catalog_model = Catalog::new().find(model).cloned();
                emit_catalog_info(&catalog_model);
                return Ok(ResolvedModel {
                    id: model.to_owned(),
                    provider: Some(provider_name.to_owned()),
                    catalog_model,
                });
            }
            // Provider named but no default_model — fall through.
            tracing::warn!(
                provider = provider_name,
                "provider specified in [agent] but has no default_model"
            );
        } else {
            tracing::warn!(
                provider = provider_name,
                "provider specified in [agent] but not found in [[providers]]"
            );
        }
    }

    // 4. First provider's default_model.
    if let Some(default) = config.provider.default_model() {
        let provider_name = config
            .provider
            .default_provider()
            .and_then(|p| p.name.as_deref())
            .unwrap_or("default");
        P::model_auto_detected(default, provider_name);
        let catalog_model = Catalog::new().find(default).cloned();
        emit_catalog_info(&catalog_model);
        return Ok(ResolvedModel {
            id: default.to_owned(),
            provider: config.provider.default_provider().and_then(|p| p.name.clone()),
            catalog_model,
        });
    }

    // 5. RHO_MODEL environment variable.
    if let Ok(env_model) = std::env::var("RHO_MODEL") {
        P::model_from_source(&env_model, "RHO_MODEL env", "auto");
        let catalog_model = Catalog::new().find(&env_model).cloned();
        emit_catalog_info(&catalog_model);
        return Ok(ResolvedModel {
            id: env_model,
            provider: None,
            catalog_model,
        });
    }

    // 6. Built-in catalog default — never errors, always works.
    let default_id = rho_ai::catalog::DEFAULT_MODEL_ID;
    P::model_from_source(default_id, "built-in default", "auto");
    let catalog_model = Catalog::new().find(default_id).cloned();
    emit_catalog_info(&catalog_model);
    Ok(ResolvedModel {
        id: default_id.to_owned(),
        provider: None,
        catalog_model,
    })
}

/// Find which configured provider should handle a given model string.
///
/// Checks providers in order for a matching `default_model`. Returns
/// `None` if no provider claims the model (it will be sent to the
/// default provider).
fn resolve_provider_for_model(model: &str, config: &RhoConfig) -> Option<String> {
    for provider in &config.provider.providers {
        if provider.default_model.as_deref() == Some(model) {
            return provider.name.clone();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::config::{ProviderConfig, ProviderSettings};

    fn config_with(model: Option<&str>, providers: Vec<ProviderConfig>) -> RhoConfig {
        RhoConfig {
            agent: rho_core::config::AgentLoopConfig {
                model: model.map(String::from),
                ..Default::default()
            },
            provider: ProviderSettings { providers },
            ..Default::default()
        }
    }

    fn config_with_provider(
        provider_name: Option<&str>,
        model: Option<&str>,
        providers: Vec<ProviderConfig>,
    ) -> RhoConfig {
        RhoConfig {
            agent: rho_core::config::AgentLoopConfig {
                model: model.map(String::from),
                provider: provider_name.map(String::from),
                ..Default::default()
            },
            provider: ProviderSettings { providers },
            ..Default::default()
        }
    }

    fn provider(name: &str, default_model: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: Some(name.to_owned()),
            default_model: default_model.map(String::from),
            ..Default::default()
        }
    }

    // ── Phase A: explicit model selection ──────────────────────────────────

    #[test]
    fn cli_model_overrides_config() {
        let config = config_with(
            Some("local-default"),
            vec![provider("local", Some("local-default"))],
        );
        let cli = Some(&"cli-model".to_owned());
        let resolved = resolve_model(&config, cli).unwrap();
        assert_eq!(resolved.id, "cli-model");
        assert_eq!(resolved.provider, None);
    }

    #[test]
    fn config_model_used_when_no_cli() {
        let config = config_with(
            Some("local-default"),
            vec![provider("local", Some("local-default"))],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "local-default");
        
        assert_eq!(resolved.provider, Some("local".to_owned()));
    }

    #[test]
    fn default_model_used_when_no_agent_model() {
        let config = config_with(None, vec![provider("local", Some("qwen2.5-coder:7b"))]);
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "qwen2.5-coder:7b");
        // Provider resolved by matching default_model.
        assert_eq!(resolved.provider, Some("local".to_owned()));
    }

    #[test]
    fn no_config_returns_default() {
        let config = config_with(None, vec![]);
        let resolved = resolve_model(&config, None).unwrap();
        // Should fall back to built-in default instead of erroring.
        assert_eq!(resolved.id, rho_ai::catalog::DEFAULT_MODEL_ID);
        assert_eq!(resolved.provider, None);
        assert!(resolved.catalog_model.is_some());
    }

    #[test]
    fn providers_without_default_model_returns_default() {
        let config = config_with(None, vec![provider("openrouter", None)]);
        let result = resolve_model(&config, None);
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.id, rho_ai::catalog::DEFAULT_MODEL_ID);
    }

    #[test]
    fn first_provider_default_model_used() {
        let config = config_with(
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "claude-sonnet-4");
        assert_eq!(resolved.provider, Some("openrouter".to_owned()));
    }

    #[test]
    fn agent_model_overrides_agent_provider() {
        let config = config_with_provider(
            Some("openai"),
            Some("claude-sonnet-4"),
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "claude-sonnet-4");
        // Provider resolved by matching default_model, ignoring agent.provider.
        assert_eq!(resolved.provider, Some("openrouter".to_owned()));
    }

    // ── Phase B: agent.provider ──────────────────────────────────────────

    #[test]
    fn agent_provider_selects_that_providers_default_model() {
        let config = config_with_provider(
            Some("openrouter"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "claude-sonnet-4");
        assert_eq!(resolved.provider, Some("openrouter".to_owned()));
    }

    #[test]
    fn agent_provider_overrides_first_provider_default_model() {
        let config = config_with_provider(
            Some("openai"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "gpt-4o");
        assert_eq!(resolved.provider, Some("openai".to_owned()));
    }

    #[test]
    fn agent_provider_unknown_falls_through() {
        let config = config_with_provider(
            Some("unknown"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "claude-sonnet-4");
        // Falls through to first provider's default_model, provider is resolved.
        assert_eq!(resolved.provider, Some("openrouter".to_owned()));
    }

    #[test]
    fn default_model_with_multiple_providers() {
        let config = config_with(
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let resolved = resolve_model(&config, None).unwrap();
        assert_eq!(resolved.id, "claude-sonnet-4");
        assert_eq!(resolved.provider, Some("openrouter".to_owned()));
    }

    // ── Catalog enrichment ─────────────────────────────────────────────

    #[test]
    fn catalog_enriches_known_model() {
        let config = config_with(Some("anthropic/claude-sonnet-4"), vec![]);
        let resolved = resolve_model(&config, None).unwrap();
        assert!(resolved.catalog_model.is_some());
        let cat = resolved.catalog_model.unwrap();
        assert_eq!(cat.id, "anthropic/claude-sonnet-4");
        assert!(cat.context_window > 0);
        assert!(cat.thinking.supported);
    }

    #[test]
    fn catalog_returns_none_for_unknown_model() {
        let config = config_with(Some("my-custom/local-model"), vec![]);
        let resolved = resolve_model(&config, None).unwrap();
        assert!(resolved.catalog_model.is_none());
    }

    #[test]
    fn default_model_has_catalog_entry() {
        let config = config_with(None, vec![]);
        let resolved = resolve_model(&config, None).unwrap();
        assert!(resolved.catalog_model.is_some());
        assert_eq!(resolved.id, rho_ai::catalog::DEFAULT_MODEL_ID);
    }

    // ── RHO_MODEL env var ───────────────────────────────────────────────
    // Note: can't test env var mutation in this crate (workspace forbids unsafe).
    // The env var code path is trivially `std::env::var("RHO_MODEL")` and tested
    // manually. See catalog tests for the non-env default model behavior.
}
