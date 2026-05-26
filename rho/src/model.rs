//! Model resolution and interactive selection.
//!
//! Resolves which model to use based on CLI flags, config, and auto-detection
//! against configured providers. Falls back to an interactive picker when
//! no model can be determined automatically.

use anyhow::Result;
use rho_core::{ProviderRegistry, RhoConfig};
use std::io::{self, BufRead, Write};

/// The minimum fuzzy similarity score to include a suggestion.
const FUZZY_THRESHOLD: f64 = 0.5;

/// Maximum number of fuzzy suggestions to display.
const MAX_SUGGESTIONS: usize = 5;

/// Popular models offered by the interactive model picker.
///
/// Each entry is (display name, provider family, model ID). The model IDs
/// use `OpenRouter`'s `provider/model` format which is the most common
/// multi-model endpoint. For direct OpenAI/Anthropic use, the user should
/// configure the right endpoint in their config.
const PICKER_MODELS: &[(&str, &str, &str)] = &[
    ("Claude Sonnet 4", "Anthropic", "anthropic/claude-sonnet-4"),
    ("GPT-4o", "OpenAI", "openai/gpt-4o"),
    ("GLM-5", "z.ai", "z-ai/glm-5"),
];

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
    // Query all providers for available models upfront (needed for
    // validation and auto-detect alike).
    let all_models = registry.list_all_models().await;

    // Build a flat list of (provider_name, model_id) strings for matching.
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
    eprintln!("auto-detected model: {model_id} (from provider: {provider_name})");
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
    // Exact match (case-sensitive).
    if let Some((provider_name, model_id)) = rho_core::find_exact(model, available) {
        eprintln!("using model from {source}: {model_id} (provider: {provider_name})");
        return model_id.to_owned();
    }

    // No models discovered — fall back to accepting the model verbatim.
    // Some providers (especially external ones) don't support `/v1/models`
    // or it may be unavailable due to auth/network issues.
    if available.is_empty() {
        eprintln!(
            "warning: could not list models from any provider; \
             accepting model from {source}: {model}"
        );
        return model.to_owned();
    }

    // Models were discovered but the specified one wasn't found —
    // show fuzzy suggestions as a warning, but still accept the model.
    // The provider may accept IDs not advertised in `/v1/models`, and
    // the API will return a proper error if the model is truly invalid.
    eprintln!("warning: model \"{model}\" not found in provider model list.");

    let suggestions = rho_core::fuzzy_match(model, available, FUZZY_THRESHOLD);
    if !suggestions.is_empty() {
        eprintln!("Did you mean:");
        eprintln!(
            "{}",
            rho_core::format_suggestions(&suggestions, MAX_SUGGESTIONS)
        );
    }

    eprintln!("continuing with model from {source}: {model}");
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

    // One or more providers are configured but none could list models.
    // Offer an interactive model picker.
    pick_model_interactively(registry)
}

/// Interactive model picker for when no models could be auto-detected.
///
/// Shows a numbered menu of popular models, plus an option to type a
/// model ID manually. Reads the user's choice from stdin and returns
/// the selected model ID.
fn pick_model_interactively(registry: &ProviderRegistry) -> Result<String> {
    let names: Vec<&str> = registry.providers().iter().map(|p| p.name()).collect();
    eprintln!();
    eprintln!("  Could not list models from: {}", names.join(", "));
    eprintln!("  Select a model to use:");
    eprintln!();

    for (i, (display_name, family, _id)) in PICKER_MODELS.iter().enumerate() {
        eprintln!(
            "    [{idx}] {name:<22} ({family})",
            idx = i + 1,
            name = display_name,
            family = family
        );
    }
    eprintln!("    [0] Enter model ID manually");
    eprintln!();
    eprint!("  Choice: ");
    io::stderr().flush().ok();

    let mut line = String::new();
    let ok = io::stdin().lock().read_line(&mut line).is_ok();
    if !ok {
        return Err(anyhow::anyhow!("could not read model choice from stdin"));
    }

    let choice = line.trim();

    // Manual entry.
    if choice == "0" {
        eprint!("  Model ID: ");
        io::stderr().flush().ok();
        let mut manual = String::new();
        if io::stdin().lock().read_line(&mut manual).is_ok() {
            let model_id = manual.trim().to_owned();
            if !model_id.is_empty() {
                eprintln!("  using model: {model_id}");
                return Ok(model_id);
            }
        }
        return Err(anyhow::anyhow!("no model ID entered"));
    }

    // Numeric selection from the list.
    if let Ok(idx) = choice.parse::<usize>()
        && idx >= 1
        && idx <= PICKER_MODELS.len()
    {
        let (display_name, _family, model_id) = PICKER_MODELS[idx - 1];
        eprintln!("  using model: {model_id} ({display_name})");
        return Ok(model_id.to_owned());
    }

    // If the user typed a model ID directly (not a number), accept it.
    if !choice.is_empty() {
        eprintln!("  using model: {choice}");
        return Ok(choice.to_owned());
    }

    Err(anyhow::anyhow!("no model selected"))
}
