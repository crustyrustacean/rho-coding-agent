//! Application assembly and startup orchestration.
//!
//! [`App`] owns the long-lived runtime state and runs the sequential setup
//! phases that `main()` delegated to it. Each phase is a private free function
//! scoped to this module, keeping [`App::build`] as a readable sequence.

use crate::cli::Cli;
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
use std::path::Path;
use tracing_subscriber::EnvFilter;

// ── App ───────────────────────────────────────────────────────────────────────

/// Long-lived runtime state for the `rho` agent.
///
/// Constructed by [`App::build`] which runs all startup phases in sequence.
/// After construction, call [`App::run`] to enter REPL mode.
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
    /// Cooperative cancellation token.
    pub(crate) cancel: Cancel,
}

impl App {
    /// Build the application from CLI arguments.
    ///
    /// Runs the full startup sequence in order. Phases marked **interactive**
    /// read from stdin and write to stdout/stderr — they require a terminal
    /// and will need to be replaced or skipped in headless (RPC) mode.
    ///
    /// # Non-interactive phases
    ///
    /// 1. Initialize tracing
    /// 2. Resolve sandbox root
    /// 3. Load configuration (two-tier TOML)
    /// 4. Construct provider registry (endpoint, auth, externality)
    /// 5. Check provider type compatibility
    /// 6. Register built-in tools
    /// 7. Compose the system prompt
    /// 8. Build agent config with CLI overrides
    /// 9. Build secret redactor
    /// 10. Determine token budget
    /// 11. Construct the session (resumed, in-memory, or persisted)
    /// 12. Log budget diagnostics
    ///
    /// # Interactive phases (stdin/stdout)
    ///
    /// - Check external provider consent (phase 6)
    /// - Scan project context files — trust workflow (phase 8)
    /// - Resolve model identifier — fallback picker (phase 10)
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

        // ── 6. Provider consent [INTERACTIVE] ────────────────────────────
        check_provider_consent(&provider_registry, &cli)?;

        // ── 7. Tool registry ────────────────────────────────────────────
        let mut tool_registry = ToolRegistry::new();
        register_all(&mut tool_registry, sandbox.clone(), &config);
        let tool_schemas = tool_registry.tool_definitions();

        // ── 8. Context files [INTERACTIVE] ──────────────────────────────
        let context_files = scan_context_files(&sandbox);

        // ── 9. System prompt ─────────────────────────────────────────────
        let system_prompt = compose_full_system_prompt(
            &sandbox,
            &context_files,
            &config,
            cli.system.as_deref(),
            cli.compact,
        );

        // ── 10. Model [INTERACTIVE] ──────────────────────────────────────
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
            cancel: Cancel::new(),
        })
    }

    /// Get the currently active provider.
    #[must_use]
    pub fn active_provider(&self) -> &dyn Provider {
        self.providers.providers()[self.active_provider_index].as_ref()
    }

    /// Run the agent in the configured mode (currently REPL only).
    ///
    /// # Errors
    ///
    /// Returns an error if the agent loop encounters a fatal error.
    pub async fn run(mut self) -> Result<()> {
        crate::repl::run_repl(&mut self).await
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

// ── Private setup phases (non-interactive) ────────────────────────────────────

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

// ── Interactive setup phases (stdin/stdout — require a terminal) ──────────────

/// Display a consent warning and read confirmation for external providers.
///
/// Shows a single consolidated prompt listing all external providers with
/// their names and endpoints. If the registry has no local provider at all
/// (only external), the warning also notes that no local model was detected.
fn check_provider_consent(registry: &ProviderRegistry, cli: &Cli) -> Result<()> {
    let external = registry.external_provider_names();

    if external.is_empty() || cli.accept_external_provider || cli.endpoint.is_some() {
        return Ok(());
    }

    let has_local = registry.providers().iter().any(|p| !p.is_external());

    eprintln!();
    if !has_local {
        eprintln!("  ⚠  No local model server detected");
    }
    eprintln!("  ⚠  External provider(s) configured:");
    for provider in registry.providers().iter().filter(|p| p.is_external()) {
        eprintln!("      - {}", provider.name());
    }
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
/// Four paths:
/// - `--continue` / `-c` — resume the most recent session for this project
/// - `--session <path>` — resume from a specific JSONL file (with stale-CWD detection)
/// - `--ephemeral` — in-memory session, no disk I/O
/// - Default — persisted session at `~/.rho/sessions/<project-hash>/`
fn build_session(
    cli: &Cli,
    model: &str,
    system_prompt: &str,
    tool_schemas: &[rho_core::ToolDefinition],
    sandbox: &SandboxRoot,
    token_budget: TokenBudget,
    redactor: Redactor,
) -> Result<Session> {
    if cli.r#continue {
        let path = rho_core::find_latest_session(sandbox.path())
            .ok_or_else(|| anyhow::anyhow!("no previous sessions found for this project"))?;
        eprintln!("resuming latest session: {}", path.display());
        resume_session(&path, model, tool_schemas, sandbox, token_budget, redactor)
    } else if let Some(ref path) = cli.session {
        eprintln!("resuming session from: {}", path.display());
        resume_session(path, model, tool_schemas, sandbox, token_budget, redactor)
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
        // Show a hint if there are previous sessions for this project.
        let previous = rho_core::list_sessions(sandbox.path());
        if !previous.is_empty() {
            eprintln!(
                "  ({} previous session(s) for this project — use rho -c to resume the latest)",
                previous.len()
            );
        }
        Ok(s)
    }
}

/// Resume a session from a JSONL file with stale-CWD detection.
///
/// Shared between `--continue` and `--session <path>`.
fn resume_session(
    path: &Path,
    model: &str,
    tool_schemas: &[rho_core::ToolDefinition],
    sandbox: &SandboxRoot,
    token_budget: TokenBudget,
    redactor: Redactor,
) -> Result<Session> {
    let mut s = Session::open(path).map_err(|e| anyhow::anyhow!("failed to open session: {e}"))?;

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
}

/// Resolve the model identifier.
///
/// Delegates to [`crate::model::resolve_model`] which handles CLI priority,
/// config fallback, auto-detection, and interactive selection.
async fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
    registry: &ProviderRegistry,
) -> Result<String> {
    crate::model::resolve_model(config, cli_model, registry).await
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
