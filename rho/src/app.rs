//! Application assembly and startup orchestration.
//!
//! [`App`] owns the long-lived runtime state and runs the sequential setup
//! phases that `main()` delegated to it. Each phase is a private free function
//! scoped to this module, keeping [`App::build`] as a readable sequence.

use crate::cli::Cli;
use crate::gate::ReplApprovalGate;
use anyhow::Result;
use rho_core::tool::CancellationToken as Cancel;
use rho_core::{
    AgentConfig, ConfigLoader, Provider, ProviderRegistry, Redactor, RhoConfig, SandboxRoot,
    Session, TokenBudget, ToolRegistry, compose_full_system_prompt,
    context_files::{ContextFile, ContextScanner, TrustStore},
    find_project_root,
};
use rho_tools::register_all;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

// ── App ───────────────────────────────────────────────────────────────────────

/// Long-lived runtime state for the `rho` agent.
///
/// Constructed by [`App::build`] which runs all startup phases in sequence.
/// After construction, call [`App::run`] to enter prompt-file or REPL mode.
pub struct App {
    /// The agent's conversation session (tree-shaped, optionally persisted).
    pub(crate) session: Session,
    /// The provider registry (owns all configured providers).
    pub(crate) providers: ProviderRegistry,
    /// Index of the active provider in the provider registry.
    pub(crate) active_provider_index: usize,
    /// Registered tools.
    pub(crate) registry: ToolRegistry,
    /// Agent loop configuration.
    pub(crate) config: AgentConfig,
    /// Approval gate for tool call confirmation.
    pub(crate) gate: ReplApprovalGate,
    /// Cooperative cancellation token.
    pub(crate) cancel: Cancel,
    /// Path to a prompt file for one-shot mode (None means REPL mode).
    pub(crate) prompt_file: Option<PathBuf>,
}

impl App {
    /// Build the application from CLI arguments.
    ///
    /// Runs the full startup sequence in order:
    ///
    /// 1. Initialize tracing
    /// 2. Resolve sandbox root
    /// 3. Load configuration (two-tier TOML)
    /// 4. Construct provider registry (endpoint, auth, externality)
    /// 5. Check provider type compatibility
    /// 6. Check external provider consent
    /// 7. Register built-in tools
    /// 8. Scan project context files (interactive trust workflow)
    /// 9. Compose the system prompt
    /// 10. Resolve the model identifier
    /// 11. Build agent config with CLI overrides
    /// 12. Build secret redactor
    /// 13. Determine token budget
    /// 14. Construct the session (resumed, in-memory, or persisted)
    /// 15. Log budget diagnostics
    ///
    /// # Errors
    ///
    /// Returns an error if any setup phase fails (bad sandbox path, unreachable
    /// model API, declined external provider consent, malformed session file).
    pub async fn build(cli: Cli) -> Result<Self> {
        // ── 1. Tracing ──────────────────────────────────────────────────
        Self::init_tracing();

        // ── 2. Sandbox root ──────────────────────────────────────────────
        let sandbox = resolve_sandbox(&cli)?;

        // ── 3. Config ────────────────────────────────────────────────────
        let config = load_config(&sandbox);

        // ── 4. Provider registry ─────────────────────────────────────────
        let provider_registry = ProviderRegistry::from_config(
            &config.provider,
            cli.endpoint.as_deref(),
            cli.api_key_env.as_deref(),
        );

        // ── 5. Provider type compatibility ──────────────────────────────
        check_provider_type(&config);

        // ── 6. Provider consent ──────────────────────────────────────────
        check_provider_consent(&provider_registry, &cli)?;

        // ── 7. Tool registry ────────────────────────────────────────────
        let mut tool_registry = ToolRegistry::new();
        register_all(&mut tool_registry, sandbox.clone(), &config);
        let tool_schemas = tool_registry.tool_schemas();

        // ── 8. Context files (interactive trust workflow) ────────────────
        let context_files = scan_context_files(&sandbox);

        // ── 9. System prompt ─────────────────────────────────────────────
        let system_prompt = compose_full_system_prompt(
            &sandbox,
            &context_files,
            &config,
            cli.system.as_deref(),
            cli.compact,
        );

        // ── 10. Model ────────────────────────────────────────────────────
        let model = resolve_model(&config, cli.model.as_ref(), &provider_registry).await?;

        // ── 11. Agent config ─────────────────────────────────────────────
        let agent_config = build_agent_config(&config, &cli);

        // ── 12. Redactor ─────────────────────────────────────────────────
        let redactor =
            Redactor::from_config(config.redaction.enabled, &config.redaction.custom_patterns);

        // ── 13. Token budget ─────────────────────────────────────────────
        let token_budget = build_token_budget(&config, &cli);

        // ── 14. Session ──────────────────────────────────────────────────
        let session = build_session(
            &cli,
            &model,
            &system_prompt,
            &tool_schemas,
            &sandbox,
            token_budget,
            redactor,
        )?;

        // ── 15. Budget diagnostics ───────────────────────────────────────
        log_budget_diagnostics(&session);

        Ok(Self {
            session,
            providers: provider_registry,
            active_provider_index: 0,
            registry: tool_registry,
            config: agent_config,
            gate: ReplApprovalGate,
            cancel: Cancel::new(),
            prompt_file: cli.prompt_file.clone(),
        })
    }

    /// Get the currently active provider.
    #[must_use]
    pub fn active_provider(&self) -> &dyn Provider {
        self.providers.providers()[self.active_provider_index].as_ref()
    }

    /// Dispatch the agent: prompt-file mode or interactive REPL.
    ///
    /// # Errors
    ///
    /// Returns an error if the prompt file cannot be read or the agent loop
    /// encounters a fatal error.
    pub async fn run(mut self) -> Result<()> {
        use crate::repl::{run_prompt_file, run_repl};

        // Check for prompt-file mode first.
        if let Some(path) = self.prompt_file.take() {
            return run_prompt_file(self, path).await;
        }

        run_repl(&mut self).await
    }

    // ── Private: tracing ────────────────────────────────────────────────

    /// Initialise the tracing subscriber.
    ///
    /// Writes structured logs to `logs/rho.log` at `INFO` level, suppressing
    /// noisy crates (`rustls`, `hyper`, `reqwest`). The log level can be
    /// overridden via the `RHO_LOG` environment variable.
    fn init_tracing() {
        let file_appender = tracing_appender::rolling::never("logs", "rho.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,rustls=warn,hyper=warn,reqwest=warn"));

        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_env_filter(env_filter)
            .init();
    }
}

// ── Private setup phases ───────────────────────────────────────────────────────

/// Resolve the sandbox root from CLI `--root` or auto-detection.
fn resolve_sandbox(cli: &Cli) -> Result<SandboxRoot> {
    match cli.root {
        Some(ref p) => SandboxRoot::new(p).map_err(|e| {
            anyhow::anyhow!("cannot establish sandbox root at `{}`: {e}", p.display())
        }),
        None => {
            find_project_root().map_err(|e| anyhow::anyhow!("cannot auto-detect project root: {e}"))
        }
    }
}

/// Load configuration from user-level and project-level TOML files.
///
/// Falls back to [`RhoConfig::default`] on error, printing a warning to stderr.
fn load_config(sandbox: &SandboxRoot) -> RhoConfig {
    ConfigLoader::load(sandbox.path()).unwrap_or_else(|e| {
        eprintln!("Warning: {e} — using defaults");
        RhoConfig::default()
    })
}

/// Warn if any configured provider type is a known non-OpenAI provider.
///
/// rho only speaks the `OpenAI` Chat Completions wire format. This check
/// fires early so the user gets a clear warning before any requests are made.
fn check_provider_type(config: &RhoConfig) {
    const NON_OPENAI: &[&str] = &[
        "anthropic",
        "google",
        "gemini",
        "cohere",
        "anyscale",
        "perplexity",
        "bedrock",
        "vertex",
    ];

    for provider_config in &config.provider.providers {
        if let Some(ref provider_type) = provider_config.r#type
            && NON_OPENAI
                .iter()
                .any(|t| provider_type.eq_ignore_ascii_case(t))
        {
            eprintln!(
                "warning: provider type \"{provider_type}\" was set, but rho only supports \
                 OpenAI-compatible endpoints. Requests may fail."
            );
        }
    }
}

/// Display a consent warning and read confirmation for external providers.
///
/// Shows a single consolidated prompt listing all external providers, not
/// one prompt per provider.
fn check_provider_consent(registry: &ProviderRegistry, cli: &Cli) -> Result<()> {
    let external = registry.external_provider_names();

    if external.is_empty() || cli.accept_external_provider || cli.endpoint.is_some() {
        return Ok(());
    }

    eprintln!();
    eprintln!("  ⚠  External provider(s) detected");
    eprintln!("      Providers: {}", external.join(", "));
    eprintln!();
    eprintln!("      Your prompts and code will be sent to external servers.");
    eprintln!("      This may expose proprietary code, secrets, or other");
    eprintln!("      sensitive data to the providers and any intermediaries.");
    eprintln!();
    eprint!("      Continue? [y/N] ");
    io::stderr().flush().ok();

    let mut line = String::new();
    let ok = io::stdin().lock().read_line(&mut line).is_ok();
    if ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        eprintln!("  Aborting. Use --accept-external-provider to skip this prompt.");
        Err(anyhow::anyhow!("user declined external provider consent"))
    }
}

/// Scan the sandbox root for project context files (interactive trust workflow).
///
/// The stdin lock is released before returning so the REPL loop can access
/// stdin without deadlock.
fn scan_context_files(sandbox: &SandboxRoot) -> Vec<ContextFile> {
    let mut trust_store = TrustStore::load_default();
    let scanner = ContextScanner::new(sandbox);
    let mut stdout = io::stdout();
    let context_files = {
        let stdin = io::stdin();
        let mut stdin_locked = stdin.lock();
        scanner.run(&mut trust_store, &mut stdin_locked, &mut stdout)
    };
    // stdin_locked is dropped here, releasing the stdin mutex.
    context_files
}

/// Build the agent loop configuration from config and CLI overrides.
fn build_agent_config(config: &RhoConfig, cli: &Cli) -> AgentConfig {
    let mut ac = AgentConfig::from_config(config);
    if let Some(max_iterations) = cli.max_iterations {
        ac.max_iterations = max_iterations;
    }
    ac
}

/// Determine the token budget from CLI or config.
fn build_token_budget(config: &RhoConfig, cli: &Cli) -> TokenBudget {
    TokenBudget::new(cli.token_budget.unwrap_or(config.agent.token_budget) as usize)
}

/// Construct the session.
///
/// Three paths:
/// - `--session <path>` — resume from a JSONL file (with stale-CWD detection)
/// - `--ephemeral` — in-memory session, no disk I/O
/// - Default — persisted session at `~/.rho/sessions/<project-hash>/`
fn build_session(
    cli: &Cli,
    model: &str,
    system_prompt: &str,
    tool_schemas: &[rho_core::ToolSchema],
    sandbox: &SandboxRoot,
    token_budget: TokenBudget,
    redactor: Redactor,
) -> Result<Session> {
    if let Some(ref path) = cli.session {
        // Resume an existing session from JSONL
        eprintln!("resuming session from: {}", path.display());
        let mut s = Session::open(path.as_path())
            .map_err(|e| anyhow::anyhow!("failed to open session: {e}"))?;

        // Detect stale CWD.
        let session_cwd = s.header().cwd.clone();
        let current_cwd = sandbox.path();
        if session_cwd != current_cwd {
            if !session_cwd.as_os_str().is_empty() && !session_cwd.exists() {
                eprintln!(
                    "warning: session's working directory no longer exists\n  \
                     session: {}\n  current: {}\n  continuing with current directory",
                    session_cwd.display(),
                    current_cwd.display()
                );
            } else {
                eprintln!(
                    "warning: session was created in a different directory\n  \
                     session: {}\n  current: {}\n  continuing with current directory",
                    session_cwd.display(),
                    current_cwd.display()
                );
            }
        }

        s.set_model(model);
        s.set_token_budget(token_budget);
        s.set_redactor(redactor);
        s.set_tools(tool_schemas.to_vec());
        Ok(s)
    } else if cli.ephemeral {
        let s = Session::in_memory(
            model,
            Some(system_prompt),
            tool_schemas.to_vec(),
            sandbox.path(),
        )
        .with_token_budget(token_budget)
        .with_redactor(redactor);
        Ok(s)
    } else {
        let s = Session::new(
            model,
            Some(system_prompt),
            tool_schemas.to_vec(),
            sandbox.path(),
        )
        .with_token_budget(token_budget)
        .with_redactor(redactor);
        if let Some(path) = s.save_path() {
            eprintln!("session: {}", path.display());
        }
        Ok(s)
    }
}

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
async fn resolve_model(
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
        let hint = if registry.external_provider_names().is_empty() {
            "Load a model in your local server and try again."
        } else {
            "Specify the model explicitly with --model or in config."
        };
        anyhow::bail!("no models available from any provider. {hint}");
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
    if let Some((provider_name, model_id)) = crate::model_match::find_exact(model, available) {
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

    let suggestions = crate::model_match::fuzzy_match(model, available, FUZZY_THRESHOLD);
    if !suggestions.is_empty() {
        eprintln!("Did you mean:");
        eprintln!(
            "{}",
            crate::model_match::format_suggestions(&suggestions, MAX_SUGGESTIONS)
        );
    }

    eprintln!("continuing with model from {source}: {model}");
    model.to_owned()
}

/// Log token budget diagnostics at startup.
fn log_budget_diagnostics(session: &Session) {
    let budget = session.token_budget();
    let system = session.system_overhead();
    let schema = session.schema_overhead();
    let total_overhead = system + schema;
    let prompt = budget.prompt_budget();
    let available = session.message_budget();

    eprintln!(
        "budget: {}T context, {}T reserve, {}T prompt \
         ({}T system + {}T schema = {}T overhead, {}T for conversation)",
        budget.context_window,
        budget.completion_reserve,
        prompt,
        system,
        schema,
        total_overhead,
        available,
    );

    if total_overhead > prompt / 2 {
        #[allow(clippy::cast_possible_truncation)]
        let pct = (100_usize.saturating_mul(total_overhead) / prompt.max(1)) as u32;
        eprintln!(
            "warning: system overhead is {pct}% of prompt budget — \
             consider --compact or increasing token_budget in .rho/config.toml"
        );
    }
}
