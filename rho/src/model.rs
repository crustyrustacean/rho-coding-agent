//! Model resolution and interactive selection.
//!
//! Resolves which model to use based on CLI flags, config, and auto-detection
//! against configured providers. Falls back to an interactive picker when
//! no model can be determined automatically.
//!
//! All output is delegated to [`crate::presenter::ReplPresenter`].

use crate::presenter::ReplPresenter as P;
use anyhow::Result;
use rho_core::{ProviderRegistry, RhoConfig};
use std::io::{self, BufRead};

/// The minimum fuzzy similarity score to include a suggestion.
const FUZZY_THRESHOLD: f64 = 0.5;

/// Maximum number of fuzzy suggestions to display.
const MAX_SUGGESTIONS: usize = 5;

/// Resolve the model identifier.
///
/// Priority: CLI `--model` → config `agent.model` → auto-detect via all
/// providers (first model found in config order).
///
/// When a model is explicitly specified (via CLI or config), it is
/// validated against the providers' model lists. If the model is not
/// found, fuzzy suggestions are shown and an error is returned.
///
/// When the model list cannot be obtained (e.g. `/v1/models` is not
/// supported or all providers are unreachable), the user-specified model
/// is accepted verbatim with a warning — this avoids blocking valid
/// workflows on providers that simply don't advertise their models.
///
/// # Interactive
///
/// If no model is specified and none can be auto-detected, this function
/// may prompt the user interactively via stdin.
pub(crate) async fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
    registry: &ProviderRegistry,
) -> Result<String> {
    let all_models = registry.list_all_models().await;

    let available: Vec<(&str, String)> = all_models
        .iter()
        .map(|(provider, info)| (*provider, info.id.clone()))
        .collect();

    // 1. CLI flag takes highest priority.
    if let Some(model) = cli_model {
        return Ok(validate_and_resolve(model, "--model", &available));
    }
    // 2. Config.
    if let Some(model) = config.agent.model.as_deref() {
        return Ok(validate_and_resolve(model, "config", &available));
    }
    // 3. Auto-detect across all providers.
    if available.is_empty() {
        return no_models_fallback(config, registry);
    }
    let (provider_name, model_id) = &available[0];
    P::model_auto_detected(model_id, provider_name);
    Ok(model_id.clone())
}

/// Validate a user-specified model against the available model list.
///
/// Three outcomes:
/// 1. Exact match found → use it, report the provider.
/// 2. Models were discovered but the specified one isn't present →
///    show fuzzy suggestions as a warning, then accept the model.
/// 3. No models could be discovered (empty list) → accept the model
///    verbatim with a warning (provider may not support `/v1/models`).
fn validate_and_resolve(model: &str, source: &str, available: &[(&str, String)]) -> String {
    if let Some((provider_name, model_id)) = rho_core::find_exact(model, available) {
        P::model_from_source(model_id, source, provider_name);
        return model_id.to_owned();
    }

    if available.is_empty() {
        P::model_accepting_verbatim(model, source);
        return model.to_owned();
    }

    P::model_not_in_list(model);

    let suggestions = rho_core::fuzzy_match(model, available, FUZZY_THRESHOLD);
    if !suggestions.is_empty() {
        P::model_suggestions(&rho_core::format_suggestions(&suggestions, MAX_SUGGESTIONS));
    }

    P::model_continuing(model, source);
    model.to_owned()
}

/// Handle the case where no models could be discovered from any provider.
///
/// Two distinct scenarios:
///
/// 1. **Zero-config** — no providers configured, only the default localhost
///    fallback exists, and it's unreachable. Returns an error with a
///    getting-started guide.
///
/// 2. **Configured but unreachable** — one or more providers are configured
///    but none could list models. Offers an interactive model picker with
///    popular models, falling back to manual entry.
///
/// # Interactive
///
/// Prompts the user via stdin to select or enter a model ID.
fn no_models_fallback(config: &RhoConfig, registry: &ProviderRegistry) -> Result<String> {
    let is_zero_config =
        config.provider.is_empty() && registry.external_provider_names().is_empty();

    if is_zero_config {
        return Err(anyhow::anyhow!(
            "\n\
             No model provider detected.\n\
             \n\
             rho could not reach a local model server and no external\n\
             provider is configured.\n\
             \n\
             To get started, either:\n\
             \n\
               1. Start a local model server (LM Studio, Ollama) on\n\
                  localhost:1234, then run rho again.\n\
             \n\
               2. Configure an external provider in ~/.rho/config.toml:\n\
             \n\
                      [provider]\n\
                      endpoint = \"https://openrouter.ai/api/v1/chat/completions\"\n\
                      api_key_env = \"OPENROUTER_API_KEY\"\n\
             \n\
                      [agent]\n\
                      model = \"gpt-4o\"\n\
             \n\
               3. Use CLI flags:\n\
             \n\
                      rho --endpoint <url> --api-key-env <VAR> --model <id>"
        ));
    }

    pick_model_interactively(registry)
}

/// Interactive model picker for when no models could be auto-detected.
///
/// Shows a numbered menu of popular models, plus an option to type a
/// model ID manually. Reads the user's choice from stdin and returns
/// the selected model ID.
fn pick_model_interactively(registry: &ProviderRegistry) -> Result<String> {
    let names: Vec<&str> = registry.providers().iter().map(|p| p.name()).collect();
    P::picker_header(&names);

    let mut line = String::new();
    let ok = io::stdin().lock().read_line(&mut line).is_ok();
    if !ok {
        return Err(anyhow::anyhow!("could not read model choice from stdin"));
    }

    let choice = line.trim();

    if choice == "0" {
        P::picker_manual_prompt();
        let mut manual = String::new();
        if io::stdin().lock().read_line(&mut manual).is_ok() {
            let model_id = manual.trim().to_owned();
            if !model_id.is_empty() {
                P::model_entered(&model_id);
                return Ok(model_id);
            }
        }
        return Err(anyhow::anyhow!("no model ID entered"));
    }

    // Numeric selection from the list (uses the same PICKER_MODELS table
    // as the presenter).
    let picker_models = P::picker_models();
    if let Ok(idx) = choice.parse::<usize>()
        && idx >= 1
        && idx <= picker_models.len()
    {
        let (display_name, _family, model_id) = picker_models[idx - 1];
        P::model_picked(model_id, display_name);
        return Ok(model_id.to_owned());
    }

    if !choice.is_empty() {
        P::model_entered(choice);
        return Ok(choice.to_owned());
    }

    Err(anyhow::anyhow!("no model selected"))
}
