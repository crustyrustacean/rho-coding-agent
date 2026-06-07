//! Application assembly and startup orchestration.
//!
//! [`App`] owns the long-lived runtime state and runs the sequential setup
//! phases that `main()` delegated to it. Each phase is a private free function
//! scoped to this module, keeping [`App::build`] as a readable sequence.

use crate::cli::Cli;
use crate::presenter::RpcPresenter as P;
use anyhow::{Context, Result};
use rho_core::tool::CancellationToken as Cancel;
use rho_core::{
    AgentConfig, ConfigLoader, LoopParams, MechanicalCompactionStrategy, Provider,
    ProviderRegistry, Redactor, RhoConfig, SandboxRoot, Session, TokenBudget, ToolRegistry,
    compose_full_system_prompt,
    context_files::{ContextFile, ContextScanner, TrustStore},
    find_project_root,
};
use rho_ext::DenoObserver;
use rho_ext::loader::ExtensionLoader;
use rho_tools::register_all;
use std::io;
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// The outcome of a single agent turn.
pub(crate) enum TurnResult {
    /// Agent produced a text reply.
    Reply(String),
    /// Agent loop encountered an error.
    Error(String),
}

// ── App ───────────────────────────────────────────────────────────────────────

/// Long-lived runtime state for the `rho` agent.
///
/// Constructed by [`App::build`] which runs all startup phases in sequence.
/// After construction, call [`App::run`] to start the RPC loop.
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
    /// Extension loader (manages TypeScript extension runtimes).
    pub(crate) ext_loader: ExtensionLoader,
    /// Extension observers (one per loaded extension).
    pub(crate) ext_observers: Vec<DenoObserver>,
    /// Keeps the tracing non-blocking writer alive until `App` is dropped.
    pub(crate) _log_guard: WorkerGuard,
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
    /// 7. Register built-in tools + extensions
    /// 8. Scan project context files (headless: auto-deny untrusted)
    /// 9. Compose the system prompt
    /// 10. Resolve model identifier
    /// 11. Build agent config with CLI overrides
    /// 12. Build secret redactor
    /// 13. Determine token budget
    /// 14. Construct the session
    /// 15. Log budget diagnostics
    ///
    /// # Errors
    ///
    /// Returns an error if any setup phase fails (bad sandbox path, unreachable
    /// model API, external provider consent required, malformed session file).
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

        // ── 6. Provider consent ─────────────────────────────────────────
        check_provider_consent(&provider_registry, &cli)?;

        // ── 7. Tool registry + extensions ────────────────────────────────
        let mut tool_registry = ToolRegistry::new();
        let session_path_holder = register_all(&mut tool_registry, sandbox.clone(), &config)
            .context("no PowerShell found on PATH — install PowerShell 7+ (pwsh) or ensure Windows PowerShell (powershell) is available")?;

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

        // ── 8. Context files (headless: auto-deny untrusted) ─────────────
        let context_files = scan_context_files(&sandbox);

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

        // ── 10. Model ─────────────────────────────────────────────────────
        let model = resolve_model(&config, cli.model.as_ref(), &provider_registry).await?;

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

    /// Switch the active model. Updates session and all extension observers.
    pub(crate) async fn set_model(&mut self, model_id: &str) {
        self.session.set_model(model_id);
        self.ext_loader.set_model_all(model_id).await;
    }

    /// Trigger context compaction on the active session.
    ///
    /// Uses mechanical compaction with a threshold of one-quarter of the message
    /// budget.
    pub(crate) async fn compact(&mut self) -> rho_core::Result<()> {
        let strategy = MechanicalCompactionStrategy::new();
        let threshold = self.session.message_budget() / 4;
        self.session
            .compact_older_than(threshold, &strategy)
            .await
            .map(|_| ())
    }

    /// Run the agent in headless RPC mode.
    ///
    /// Fires extension `onLoad` hooks, then starts the JSONL command loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC loop encounters a fatal I/O failure.
    pub async fn run(self) -> Result<()> {
        self.ext_loader.fire_on_load().await;
        crate::rpc::run_rpc(self).await
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

// ── Shared orchestration (free function) ─────────────────────────────────────

/// Run one agent turn.
///
/// Delegates to [`rho_core::run_loop`] with the given [`LoopParams`] and wraps
/// the result into a [`TurnResult`].
pub(crate) async fn run_agent_turn(
    session: &mut Session,
    message: &str,
    params: &LoopParams<'_>,
) -> TurnResult {
    match rho_core::run_loop(session, message, params).await {
        Ok(reply) => TurnResult::Reply(reply),
        Err(e) => TurnResult::Error(e.to_string()),
    }
}

// ── Private setup phases ─────────────────────────────────────────────────────

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

/// Check external provider consent.
///
/// If external providers are configured and neither `--accept-external-provider`
/// nor `--endpoint` is passed, exits with an error. Headless mode cannot
/// interactively prompt for consent.
fn check_provider_consent(registry: &ProviderRegistry, cli: &Cli) -> Result<()> {
    if cli.accept_external_provider || cli.endpoint.is_some() {
        return Ok(());
    }

    let external = registry.external_provider_names();
    if external.is_empty() {
        return Ok(());
    }

    let has_local = registry.providers().iter().any(|p| !p.is_external());
    let external_names: Vec<&str> = registry
        .providers()
        .iter()
        .filter(|p| p.is_external())
        .map(|p| p.name())
        .collect();

    P::provider_consent_prompt(has_local, &external_names);
    P::provider_consent_aborted();
    Err(anyhow::anyhow!(
        "external provider consent required; \
         pass --accept-external-provider to proceed"
    ))
}

/// Scan the sandbox root for project context files.
///
/// Silently includes already-trusted files; auto-denies new or changed
/// files without blocking on stdin. This is the safe policy for headless
/// mode: the client cannot answer trust prompts, and untrusted project
/// instructions must not be injected unattended.
fn scan_context_files(sandbox: &SandboxRoot) -> Vec<ContextFile> {
    let mut trust_store = TrustStore::load_default();
    let scanner = ContextScanner::new(sandbox);

    // Empty reader → prompt_yn always returns false (auto-deny).
    // Sink writer  → trust prompts and file contents are discarded.
    // Already-trusted files (hash match) load silently without any I/O.
    let mut empty = io::Cursor::new(&b""[..]);
    let mut null = io::sink();
    scanner.run(&mut trust_store, &mut empty, &mut null)
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
/// Delegates to [`crate::model::resolve_model`].
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

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let user_ext = home.join(".rho").join("extensions");
    if user_ext.is_dir() {
        dirs.push(user_ext);
    }

    let project_ext = sandbox_path.join(".rho").join("extensions");
    if project_ext.is_dir() && !dirs.contains(&project_ext) {
        dirs.push(project_ext);
    }

    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_result_variants() {
        let reply = TurnResult::Reply("ok".into());
        assert!(matches!(reply, TurnResult::Reply(_)));

        let err = TurnResult::Error("fail".into());
        assert!(matches!(err, TurnResult::Error(_)));
        let TurnResult::Error(msg) = err else {
            panic!("expected Error variant");
        };
        assert_eq!(msg, "fail");
    }
}
