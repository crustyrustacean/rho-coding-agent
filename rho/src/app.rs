//! Application assembly and startup orchestration.
//!
//! [`App`] owns the long-lived runtime state and runs the sequential setup
//! phases that `main()` delegated to it. Each phase is a private free function
//! scoped to this module, keeping [`App::build`] as a readable sequence.

use crate::cli::{Cli, Mode};
use crate::presenter::ReplPresenter as P;
use crate::presenter::RpcPresenter;
use anyhow::Result;
use rho_core::tool::CancellationToken as Cancel;
use rho_core::{
    AgentConfig, ConfigLoader, Provider, ProviderRegistry, Redactor, RhoConfig, SandboxRoot,
    Session, TokenBudget, ToolRegistry, compose_full_system_prompt,
    context_files::{ContextFile, ContextScanner, TrustStore},
    find_project_root,
};
use rho_ext::DenoObserver;
use rho_ext::loader::ExtensionLoader;
use rho_tools::register_all;
use std::io::{self, BufRead};
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

// ── App ───────────────────────────────────────────────────────────────────────

/// Long-lived runtime state for the `rho` agent.
///
/// Constructed by [`App::build`] which runs all startup phases in sequence.
/// After construction, call [`App::run`] to dispatch to the selected mode.
pub struct App {
    /// Execution mode selected via `--mode` (default: [`Mode::Repl`]).
    pub(crate) mode: Mode,
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
    /// Extension loader (manages TypeScript extension runtimes).
    pub(crate) ext_loader: ExtensionLoader,
    /// Extension observers (one per loaded extension).
    pub(crate) ext_observers: Vec<DenoObserver>,
    /// Keeps the tracing non-blocking writer alive until `App` is dropped.
    /// Without this, the worker thread flushes and exits during `init_tracing`,
    /// silently dropping all subsequent log events.
    pub(crate) _log_guard: WorkerGuard,
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
        let _log_guard = Self::init_tracing();

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
        let session_path_holder = register_all(&mut tool_registry, sandbox.clone(), &config);

        // ── 7b. Extensions ───────────────────────────────────────────────
        let mut ext_loader = ExtensionLoader::new(
            config.extensions.clone(),
            sandbox.path().to_path_buf(),
            rho_core::denylist::CommandDenylist::from_config(&config),
        );
        let ext_dirs = extension_dirs(sandbox.path());
        if !ext_dirs.is_empty() {
            if let Err(e) = ext_loader.load_all(&ext_dirs) {
                P::extension_load_error(&e.to_string());
            }
            ext_loader.register_tools(&mut tool_registry);
            P::extensions_loaded(ext_loader.len());
        }
        let ext_observers = ext_loader.build_observers();

        let tool_schemas = tool_registry.tool_definitions();

        // Whether we are running in a headless mode (no interactive stdin).
        // Determined once here and threaded through all remaining interactive phases.
        let headless = cli.mode == Mode::Rpc;

        // ── 8. Context files [INTERACTIVE] ──────────────────────────────
        let context_files = scan_context_files(&sandbox, headless);

        // ── 9. System prompt ─────────────────────────────────────────────
        let mut system_prompt = compose_full_system_prompt(
            &sandbox,
            &context_files,
            &config,
            cli.system.as_deref(),
            cli.compact,
        );

        // Inject extension awareness so the model knows about loaded extensions.
        if !ext_loader.is_empty() {
            use std::fmt::Write;
            let _ = write!(system_prompt, "\n\n# Loaded Extensions\n\n");
            system_prompt.push_str(
                "The following extensions are active and have contributed tools \
                 to your tool registry. You may use these tools normally.\n",
            );
            for (ext_name, tool_names) in ext_loader.extension_tools() {
                let _ = write!(system_prompt, "\n- **{ext_name}**: ");
                if tool_names.is_empty() {
                    system_prompt.push_str("(no tools)\n");
                } else {
                    system_prompt.push('`');
                    system_prompt.push_str(&tool_names.join("`, `"));
                    system_prompt.push('`');
                    system_prompt.push('\n');
                }
            }
            system_prompt.push_str(
                "\nExtension tools appear in your tool definitions alongside \
                 built-in tools. Use them the same way.\n",
            );
        }

        // ── 10. Model [INTERACTIVE] ──────────────────────────────────────
        let model =
            resolve_model(&config, cli.model.as_ref(), &provider_registry, headless).await?;

        // Set model in all loaded extensions so rho.getModel() works.
        ext_loader.set_model_all(&model).await;

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
            &session_path_holder,
        )?;

        // ── 15. Budget diagnostics ───────────────────────────────────────
        log_budget_diagnostics(&session);

        Ok(Self {
            mode: cli.mode,
            session,
            providers: provider_registry,
            active_provider_index: 0,
            registry: tool_registry,
            config: agent_config,
            cancel: Cancel::new(),
            ext_loader,
            ext_observers,
            #[allow(clippy::used_underscore_binding)]
            _log_guard,
        })
    }

    /// Get the currently active provider.
    #[must_use]
    pub fn active_provider(&self) -> &dyn Provider {
        self.providers.providers()[self.active_provider_index].as_ref()
    }

    /// Run the agent in the mode selected via `--mode`.
    ///
    /// - [`Mode::Repl`] — interactive terminal session (default).
    /// - [`Mode::Rpc`] — headless JSONL over stdin/stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent loop encounters a fatal error.
    pub async fn run(mut self) -> Result<()> {
        // Fire extension onLoad hooks now that the app is fully built.
        self.ext_loader.fire_on_load().await;

        match self.mode {
            Mode::Repl => crate::repl::run_repl(&mut self).await,
            Mode::Rpc => crate::rpc::run_rpc(self).await,
        }
    }

    // ── Private: tracing ────────────────────────────────────────────────

    /// Initialise the tracing subscriber.
    ///
    /// Writes structured logs to `logs/rho.log` at `INFO` level, suppressing
    /// noisy crates (`rustls`, `hyper`, `reqwest`). The log level can be
    /// overridden via the `RHO_LOG` environment variable.
    fn init_tracing() -> WorkerGuard {
        let file_appender = tracing_appender::rolling::never("logs", "rho.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,rustls=warn,hyper=warn,reqwest=warn"));

        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_env_filter(env_filter)
            .init();

        guard
    }
}

// ── Private setup phases (non-interactive) ────────────────────────────────────

/// Resolve the sandbox root from CLI `--root` or auto-detection.
fn resolve_sandbox(cli: &Cli) -> Result<SandboxRoot> {
    if let Some(ref p) = cli.root {
        SandboxRoot::new(p)
            .map_err(|e| anyhow::anyhow!("cannot establish sandbox root at `{}`: {e}", p.display()))
    } else {
        let (root, found_marker) = find_project_root()
            .map_err(|e| anyhow::anyhow!("cannot auto-detect project root: {e}"))?;
        if !found_marker {
            P::config_warning(&format!(
                "no project marker found in {} or any parent directory -- using current directory as sandbox root",
                root.path().display()
            ));
        }
        Ok(root)
    }
}

/// Load configuration from user-level and project-level TOML files.
///
/// Falls back to [`RhoConfig::default`] on error, printing a warning to stderr.
fn load_config(sandbox: &SandboxRoot) -> RhoConfig {
    ConfigLoader::load(sandbox.path()).unwrap_or_else(|e| {
        P::config_warning(&e.to_string());
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
            P::provider_type_warning(provider_type);
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
    let external_names: Vec<&str> = registry
        .providers()
        .iter()
        .filter(|p| p.is_external())
        .map(|p| p.name())
        .collect();

    // In headless (RPC) mode skip interactive stdin — the caller must pass
    // --accept-external-provider explicitly. Full JSONL-based consent handling
    // arrives in step 6.
    if cli.mode == Mode::Rpc {
        RpcPresenter::provider_consent_prompt(has_local, &external_names);
        RpcPresenter::provider_consent_aborted();
        return Err(anyhow::anyhow!(
            "external provider consent required in RPC mode; \
             pass --accept-external-provider to proceed"
        ));
    }

    P::provider_consent_prompt(has_local, &external_names);

    let mut line = String::new();
    let ok = io::stdin().lock().read_line(&mut line).is_ok();
    if ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        P::provider_consent_aborted();
        Err(anyhow::anyhow!("user declined external provider consent"))
    }
}

/// Scan the sandbox root for project context files.
///
/// **Interactive mode** — displays file contents and prompts the user to trust
/// new or changed files via stdin/stdout.
///
/// **Headless mode** (`headless = true`) — silently includes already-trusted
/// files; auto-denies new or changed files without blocking on stdin. This is
/// the safe policy for RPC mode: the client cannot answer trust prompts, and
/// untrusted project instructions must not be injected unattended.
///
/// In interactive mode the stdin lock is released before returning so the
/// REPL loop can access stdin without deadlock.
fn scan_context_files(sandbox: &SandboxRoot, headless: bool) -> Vec<ContextFile> {
    let mut trust_store = TrustStore::load_default();
    let scanner = ContextScanner::new(sandbox);

    if headless {
        // Empty reader → prompt_yn always returns false (auto-deny).
        // Sink writer  → trust prompts and file contents are discarded.
        // Already-trusted files (hash match) load silently without any I/O.
        let mut empty = io::Cursor::new(&b""[..]);
        let mut null = io::sink();
        scanner.run(&mut trust_store, &mut empty, &mut null)
    } else {
        let mut stdout = io::stdout();
        let context_files = {
            let stdin = io::stdin();
            let mut stdin_locked = stdin.lock();
            scanner.run(&mut trust_store, &mut stdin_locked, &mut stdout)
        };
        // stdin_locked is dropped here, releasing the stdin mutex.
        context_files
    }
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
    let context_window = cli.token_budget.unwrap_or(config.agent.token_budget) as usize;
    let completion_reserve = config.agent.completion_reserve as usize;
    TokenBudget::with_reserve(context_window, completion_reserve)
}

/// Construct the session.
///
/// Four paths:
/// - `--continue` / `-c` — resume the most recent session for this project
/// - `--session <path>` — resume from a specific JSONL file (with stale-CWD detection)
/// - `--ephemeral` — in-memory session, no disk I/O
/// - Default — persisted session at `~/.rho/sessions/<project-hash>/`
#[allow(clippy::too_many_arguments)]
fn build_session(
    cli: &Cli,
    model: &str,
    system_prompt: &str,
    tool_schemas: &[rho_core::ToolDefinition],
    sandbox: &SandboxRoot,
    token_budget: TokenBudget,
    redactor: Redactor,
    session_path_holder: &rho_tools::SessionPathHolder,
) -> Result<Session> {
    if cli.r#continue {
        let path = rho_core::find_latest_session(sandbox.path())
            .ok_or_else(|| anyhow::anyhow!("no previous sessions found for this project"))?;
        P::session_resumed(&path);
        resume_session(
            &path,
            model,
            tool_schemas,
            sandbox,
            token_budget,
            redactor,
            session_path_holder,
        )
    } else if let Some(ref path) = cli.session {
        P::session_resumed(path);
        resume_session(
            path,
            model,
            tool_schemas,
            sandbox,
            token_budget,
            redactor,
            session_path_holder,
        )
    } else if cli.ephemeral {
        let s = Session::in_memory(
            model,
            Some(system_prompt),
            tool_schemas.to_vec(),
            sandbox.path(),
        )
        .with_token_budget(token_budget)
        .with_redactor(redactor);
        // Ephemeral sessions have no path — holder stays None.
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
            P::session_created(path);
            rho_tools::SessionSummary::set_path(session_path_holder, path.to_path_buf());
        }
        // Show a hint if there are previous sessions for this project.
        let previous = rho_core::list_sessions(sandbox.path());
        if !previous.is_empty() {
            P::previous_sessions_hint(previous.len());
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
    session_path_holder: &rho_tools::SessionPathHolder,
) -> Result<Session> {
    let mut s = Session::open(path).map_err(|e| anyhow::anyhow!("failed to open session: {e}"))?;

    // Detect stale CWD.
    let session_cwd = s.header().cwd.clone();
    let current_cwd = sandbox.path();
    if session_cwd != current_cwd {
        P::stale_cwd_warning(&session_cwd, current_cwd, session_cwd.exists());
    }

    s.set_model(model);
    s.set_token_budget(token_budget);
    s.set_redactor(redactor);
    s.set_tools(tool_schemas.to_vec());
    rho_tools::SessionSummary::set_path(session_path_holder, path.to_path_buf());
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
    headless: bool,
) -> Result<String> {
    crate::model::resolve_model(config, cli_model, registry, headless).await
}

/// Log token budget diagnostics at startup.
fn log_budget_diagnostics(session: &Session) {
    let budget = session.token_budget();
    let system = session.system_overhead();
    let schema = session.schema_overhead();
    let total_overhead = system + schema;
    let prompt = budget.prompt_budget();
    let available = session.message_budget();

    P::budget_summary(
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
        P::budget_overhead_warning(pct);
    }
}

/// Build the list of extension directories to scan.
///
/// Checks for:
/// - User-level: `~/.rho/extensions/`
/// - Project-level: `.rho/extensions/` (relative to sandbox root)
pub fn extension_dirs(sandbox_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    // User-level extensions.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let user_ext = home.join(".rho").join("extensions");
    if user_ext.is_dir() {
        dirs.push(user_ext);
    }

    // Project-level extensions.
    let project_ext = sandbox_path.join(".rho").join("extensions");
    if project_ext.is_dir() && !dirs.contains(&project_ext) {
        dirs.push(project_ext);
    }

    dirs
}
