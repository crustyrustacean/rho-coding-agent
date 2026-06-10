//! Model resolution.
//!
//! Resolves which model to use based on CLI flags and config.
//! Config is the source of truth — no network calls at startup.
//! Discovery (`/v1/models`) is reserved for runtime commands
//! (`/models`, `/model` browsing).

use crate::presenter::RpcPresenter as P;
use anyhow::Result;
use rho_core::RhoConfig;

/// Resolve the model identifier without network calls.
///
/// Priority (first match wins):
///
/// 1. CLI `--model` — use verbatim
/// 2. Config `agent.model` — use verbatim
/// 3. Config `agent.provider` + that provider's `default_model` — use it
/// 4. First provider's `default_model` — use it
/// 5. None of the above — return an error
///
/// Model names are accepted as-is. Typos and misconfiguration surface
/// as clear HTTP errors at request time, not as startup blocking calls.
/// This matches the pi pattern: static declaration, request-time validation.
pub(crate) fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
) -> Result<(String, Option<String>)> {
    // 1. CLI flag takes highest priority.
    if let Some(model) = cli_model {
        P::model_from_source(model, "--model", "cli");
        return Ok((model.clone(), None));
    }

    // 2. Config agent.model.
    if let Some(model) = config.agent.model.as_deref() {
        let provider = resolve_provider_for_model(model, config);
        P::model_from_source(model, "config", provider.as_deref().unwrap_or("default"));
        return Ok((model.to_owned(), provider));
    }

    // 3. Config agent.provider + that provider's default_model.
    //    (Future: when `agent.provider` field is added to config.)
    //    For now, skip — will be added in Phase B.

    // 4. First provider's default_model.
    if let Some(default) = config.provider.default_model() {
        let provider_name = config
            .provider
            .default_provider()
            .and_then(|p| p.name.as_deref())
            .unwrap_or("default");
        P::model_auto_detected(default, provider_name);
        return Ok((default.to_owned(), None));
    }

    // 5. Nothing configured.
    no_model_configured(config)
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

/// Handle the case where no model is configured.
fn no_model_configured(config: &RhoConfig) -> Result<(String, Option<String>)> {
    let has_providers = !config.provider.is_empty();

    if has_providers {
        // Providers are configured but none has a default_model.
        let provider_hint = config
            .provider
            .default_provider()
            .and_then(|p| p.name.as_deref())
            .unwrap_or("provider1");
        Err(anyhow::anyhow!(
            "\n\
             No model configured.\n\
             \n\
             Providers are configured but no model is specified.\n\
             \n\
             To fix this, either:\n\
             \n\
               1. Add `default_model` to a provider in ~/.rho/config.toml:\n\
             \n\
                      [[providers]]\n\
                      name = \"{provider_hint}\"\n\
                      default_model = \"gpt-4o\"\n\
             \n\
               2. Set a model in config:\n\
             \n\
                      [agent]\n\
                      model = \"gpt-4o\"\n\
             \n\
               3. Pass --model on the command line:\n\
             \n\
                      rho --model gpt-4o"
        ))
    } else {
        Err(anyhow::anyhow!(
            "\n\
             No model provider or model configured.\n\
             \n\
             To get started, configure a provider in ~/.rho/config.toml:\n\
             \n\
               [[providers]]\n\
               name = \"openrouter\"\n\
               preset = \"openrouter\"\n\
               api_key_env = \"OPENROUTER_API_KEY\"\n\
               default_model = \"anthropic/claude-sonnet-4\"\n\
             \n\
             Or use CLI flags:\n\
             \n\
               rho --endpoint <url> --model <id>"
        ))
    }
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

    fn provider(name: &str, default_model: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            name: Some(name.to_owned()),
            endpoint: Some("http://localhost:1234/v1/chat/completions".to_owned()),
            default_model: default_model.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn cli_model_wins_over_config() {
        let config = config_with(
            Some("config-model"),
            vec![provider("local", Some("local-default"))],
        );
        let cli = Some(&"cli-model".to_owned());
        let (model, provider) = resolve_model(&config, cli).unwrap();
        assert_eq!(model, "cli-model");
        assert_eq!(provider, None);
    }

    #[test]
    fn config_model_used_when_no_cli() {
        let config = config_with(
            Some("local-default"),
            vec![provider("local", Some("local-default"))],
        );
        let (model, provider) = resolve_model(&config, None).unwrap();
        assert_eq!(model, "local-default");
        // Provider is resolved by matching default_model.
        assert_eq!(provider, Some("local".to_owned()));
    }

    #[test]
    fn default_model_used_when_no_agent_model() {
        let config = config_with(None, vec![provider("local", Some("qwen2.5-coder:7b"))]);
        let (model, provider) = resolve_model(&config, None).unwrap();
        assert_eq!(model, "qwen2.5-coder:7b");
        assert_eq!(provider, None);
    }

    #[test]
    fn no_config_returns_error() {
        let config = config_with(None, vec![]);
        let result = resolve_model(&config, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No model provider or model configured"));
    }

    #[test]
    fn providers_without_default_model_returns_error() {
        let config = config_with(None, vec![provider("local", None)]);
        let result = resolve_model(&config, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No model configured"));
        assert!(err.contains("default_model"));
    }

    #[test]
    fn provider_resolved_for_matching_default_model() {
        let config = config_with(
            Some("claude-sonnet-4"),
            vec![
                provider("local", Some("qwen2.5-coder:7b")),
                provider("openrouter", Some("claude-sonnet-4")),
            ],
        );
        let (model, provider) = resolve_model(&config, None).unwrap();
        assert_eq!(model, "claude-sonnet-4");
        assert_eq!(provider, Some("openrouter".to_owned()));
    }

    #[test]
    fn first_provider_default_model_used() {
        let config = config_with(
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("local", Some("qwen2.5-coder:7b")),
            ],
        );
        let (model, provider) = resolve_model(&config, None).unwrap();
        assert_eq!(model, "claude-sonnet-4");
        assert_eq!(provider, None);
    }
}
