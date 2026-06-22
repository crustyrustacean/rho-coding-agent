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
    AgentConfig, ConfigLoader, LoopParams, MechanicalCompactionStrategy, OpenAiCompatibleProvider,
    Provider, ProviderRegistry, Redactor, RhoConfig, SandboxRoot, Session, TokenBudget,
    ToolRegistry, compose_full_system_prompt,
    context_files::{ContextFile, ContextScanner, TrustStore},
    find_project_root,
};
use rho_ext::DenoObserver;
use rho_ext::loader::ExtensionLoader;
use rho_memory::Memory;
use rho_tools::register_all;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// The outcome of a single agent turn.
pub(crate) enum TurnResult {
    /// Agent produced a structured result.
    Done(Box<rho_core::AgentResult>),
    /// Agent loop encountered an error.
    Error(String),
}

/// Why a runtime model switch was rejected.
///
/// Returned by [`App::set_model`]. A rejected switch leaves the session's
/// active model and provider untouched, so the next prompt continues to use
/// the previously working model instead of failing at request time with an
/// opaque HTTP error.
#[derive(Debug)]
pub(crate) enum SetModelError {
    /// A bare model id was not advertised by any provider's `/v1/models`.
    ModelNotFound {
        /// The rejected model identifier.
        model: String,
    },
    /// `provider:model` syntax named a provider that isn't configured.
    UnknownProvider {
        /// The rejected provider name.
        provider: String,
        /// The model identifier that was to be sent to it.
        model: String,
    },
}

impl SetModelError {
    /// Human-readable message suitable for surfacing to the user (e.g. as a
    /// JSON-RPC error message).
    pub(crate) fn message(&self) -> String {
        match self {
            Self::ModelNotFound { model } => format!(
                "model '{model}' was not found on any provider; use /models to list \
                 available models, or 'provider:{model}' to target a provider that \
                 does not advertise models via /v1/models"
            ),
            Self::UnknownProvider { provider, model } => format!(
                "unknown provider '{provider}' for model '{model}'; use /providers to \
                 list configured providers"
            ),
        }
    }
}

// ── Session construction ─────────────────────────────────────────────────────

/// Configuration for session construction.
///
/// Collects all parameters needed to build a [`Session`] from the four
/// construction modes (continue, resume, ephemeral, persisted).
struct SessionConfig {
    /// Model identifier.
    model: String,
    /// Composed system prompt.
    system_prompt: String,
    /// Tool definitions for the session.
    tool_schemas: Vec<rho_core::ToolDefinition>,
    /// Project sandbox root.
    sandbox: SandboxRoot,
    /// Token budget (context window + completion reserve).
    token_budget: TokenBudget,
    /// Secret redactor.
    redactor: Redactor,
    /// Optional reasoning effort level.
    reasoning_effort: Option<String>,
    /// Shared session path holder (for `SessionSummary` tool).
    session_path_holder: rho_tools::SessionPathHolder,
    /// Construction mode (continue, resume, ephemeral, persisted).
    mode: SessionMode,
}

/// Session construction mode.
enum SessionMode {
    /// Resume the most recent session for this project.
    Continue,
    /// Resume from a specific JSONL file.
    Resume(PathBuf),
    /// In-memory session, no disk I/O.
    Ephemeral,
    /// New persisted session at `~/.rho/sessions/<project-hash>/`.
    Persisted,
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
    /// 5. Resolve model identifier; if the built-in default model is
    ///    selected, synthesize a matching `OpenRouter` provider
    /// 6. Check provider type compatibility
    /// 7. Check external provider consent
    /// 8. Register built-in tools + extensions
    /// 9. Scan project context files (headless: auto-deny untrusted)
    /// 10. Compose the system prompt
    /// 11. Propagate model id to extensions
    /// 12. Build agent config with CLI overrides
    /// 13. Build secret redactor
    /// 14. Determine token budget
    /// 15. Resolve reasoning effort (gated by thinking support)
    /// 16. Construct the session
    /// 17. Log budget diagnostics
    ///
    /// # Errors
    ///
    /// Returns an error if any setup phase fails (bad sandbox path, unreachable
    /// model API, external provider consent required, malformed session file).
    #[allow(clippy::too_many_lines)]
    pub async fn build(cli: Cli) -> Result<Self> {
        // ── 1. Tracing ──────────────────────────────────────────────────
        let _log_guard = Self::init_tracing();

        // ── 2. Sandbox root ──────────────────────────────────────────────
        let sandbox = resolve_sandbox(&cli)?;

        // ── 3. Config ────────────────────────────────────────────────────
        let config = load_config(&sandbox);

        // ── 4. Provider registry ─────────────────────────────────────────
        let mut provider_registry = ProviderRegistry::from_config(
            &config.provider,
            cli.endpoint.as_deref(),
            cli.api_key_env.as_deref(),
        );

        // ── 5. Model resolution + default provider synthesis ─────────────
        // Resolve the model *before* the provider-consent gate so that, when
        // the built-in default model (`anthropic/claude-sonnet-4`, an
        // OpenRouter model id) is selected by the zero-config fallback, we
        // can register a matching OpenRouter provider first. Without this,
        // the default model would be routed to the `localhost:1234` zero-config
        // provider and fail at request time with a connection error.
        let resolved = resolve_model(&config, cli.model.as_ref());
        let active_provider_index = if resolved.is_builtin_default {
            ensure_openrouter_provider(&mut provider_registry)
        } else {
            resolved
                .provider
                .as_deref()
                .and_then(|name| provider_registry.index_of(name))
                .unwrap_or(0)
        };
        let model = resolved.id.clone();

        // ── 6. Provider type compatibility ──────────────────────────────
        check_provider_type(&config);

        // ── 7. Provider consent ─────────────────────────────────────────
        check_provider_consent(&provider_registry, &cli)?;

        // ── 8. Tool registry + memory + extensions ─────────────────
        let mut tool_registry = ToolRegistry::new();
        let memory = open_memory(&sandbox, &config);
        let session_path_holder =
            register_all(&mut tool_registry, sandbox.clone(), &config, memory.clone())
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

        // ── 9. Context files (headless: auto-deny untrusted) ─────────────
        let context_files = scan_context_files(&sandbox);

        // ── 10. System prompt ────────────────────────────────────────────
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

        // ── 11. Model (already resolved in phase 5) ──────────────────────
        // `resolved`, `model`, and `active_provider_index` were computed
        // before the consent gate so the OpenRouter provider could be
        // synthesized for the built-in default. Here we only propagate the
        // model id to extensions.
        //
        // Set model in all loaded extensions so rho.getModel() works.
        ext_loader.set_model_all(&model).await;

        // ── 12. Agent config ─────────────────────────────────────────────
        let agent_config = build_agent_config(&config, &cli);

        // ── 13. Redactor ─────────────────────────────────────────────────
        let redactor =
            Redactor::from_config(config.redaction.enabled, &config.redaction.custom_patterns);

        // ── 14. Token budget ─────────────────────────────────────────────
        // Use catalog-derived context window when available, falling back to config.
        let token_budget = build_token_budget(&config, &cli, resolved.catalog_model.as_ref());
        tracing::info!(
            context_window = token_budget.context_window,
            completion_reserve = token_budget.completion_reserve,
            "token budget resolved"
        );

        // ── 15. Reasoning effort ────────────────────────────────────────
        // Only set reasoning effort if the model supports thinking.
        // If the model doesn't support thinking, suppress reasoning_effort
        // even if the user configured it, to avoid sending invalid parameters.
        let reasoning_effort = match (&resolved.catalog_model, &config.agent.reasoning_effort) {
            (Some(catalog), Some(effort)) if catalog.thinking.supported => Some(effort.clone()),
            (Some(_catalog), _effort) => None,
            (None, effort) => effort.clone(),
        };
        match (&reasoning_effort, &resolved.catalog_model) {
            (Some(e), Some(_c)) => {
                tracing::info!(effort = %e, "reasoning effort enabled (model supports thinking)");
            }
            (None, Some(c)) if c.thinking.supported => {
                tracing::info!("reasoning effort not configured");
            }
            (None, Some(_c)) => {
                tracing::info!("reasoning effort suppressed (model does not support thinking)");
            }
            _ => {}
        }

        // ── 16. Session ──────────────────────────────────────────────────
        let session_mode = if cli.r#continue {
            SessionMode::Continue
        } else if let Some(ref path) = cli.session {
            SessionMode::Resume(path.clone())
        } else if cli.ephemeral {
            SessionMode::Ephemeral
        } else {
            SessionMode::Persisted
        };

        let session_config = SessionConfig {
            model,
            system_prompt,
            tool_schemas,
            sandbox,
            token_budget,
            redactor,
            reasoning_effort,
            session_path_holder,
            mode: session_mode,
        };

        let session = build_session(session_config)?;

        // ── 17. Budget diagnostics ───────────────────────────────────────
        log_budget_diagnostics(&session);

        Ok(Self {
            session,
            providers: provider_registry,
            active_provider_index,
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

    /// Switch the active model.
    ///
    /// Accepts a bare model ID or `provider:model` syntax:
    ///
    /// - **Bare `model_id`**: Discovers which provider serves the model via
    ///   `/v1/models`, switches `active_provider_index`, then updates the
    ///   session and extension observers.
    ///
    /// - **`provider:model_id`**: Switches to the named provider directly
    ///   (no model discovery), then sets the model string. Useful when the
    ///   target provider is slow to respond or doesn't support `/v1/models`.
    ///
    /// # Rejections
    ///
    /// A switch is **rejected** (returning [`SetModelError`]) and the session
    /// is left untouched when the model cannot be routed to a real provider:
    ///
    /// - Bare `model_id` not advertised by any provider's `/v1/models`.
    /// - `provider:model_id` where the named provider is not configured.
    ///
    /// Rejecting (rather than accepting the string verbatim) prevents the
    /// frontend from reporting a successful switch that only fails on the
    /// next prompt with an opaque HTTP error. Note that `provider:model_id`
    /// with a *known* provider still trusts the user's assertion that the
    /// model exists there, since discovery is intentionally skipped on that
    /// path.
    pub(crate) async fn set_model(&mut self, spec: &str) -> Result<(), SetModelError> {
        // Parse optional `provider:model` syntax.
        let (explicit_provider, model_id) = if let Some((provider, model)) = spec.split_once(':') {
            (Some(provider), model)
        } else {
            (None, spec)
        };

        if let Some(provider_name) = explicit_provider {
            // Explicit provider selection — switch by name without model discovery.
            // A known provider is trusted (the model string is set as-is, useful
            // when the provider is slow or doesn't support /v1/models), but an
            // unknown provider name is a hard error: there is nothing to route
            // the request to.
            if let Some(index) = self.providers.index_of(provider_name) {
                let old_provider = self.active_provider().name().to_owned();
                self.active_provider_index = index;
                let new_provider = self.active_provider().name().to_owned();
                if old_provider != new_provider {
                    tracing::info!(
                        old_provider = %old_provider,
                        new_provider = %new_provider,
                        model = %model_id,
                        "switched provider explicitly"
                    );
                }
            } else {
                tracing::warn!(
                    provider = %provider_name,
                    model = %model_id,
                    "unknown provider; model switch rejected"
                );
                return Err(SetModelError::UnknownProvider {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                });
            }
        } else if let Some(index) = self.providers.find_model_index(model_id).await {
            let old_provider = self.active_provider().name().to_owned();
            self.active_provider_index = index;
            let new_provider = self.active_provider().name().to_owned();
            if old_provider != new_provider {
                tracing::info!(
                    old_provider = %old_provider,
                    new_provider = %new_provider,
                    model = %model_id,
                    "switched provider for model"
                );
            }
        } else {
            tracing::warn!(
                model = %model_id,
                "model not found on any provider via /v1/models; model switch rejected"
            );
            return Err(SetModelError::ModelNotFound {
                model: model_id.to_owned(),
            });
        }

        self.session.set_model(model_id);
        self.ext_loader.set_model_all(model_id).await;
        Ok(())
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
        Ok(result) => TurnResult::Done(Box::new(result)),
        Err(e) => TurnResult::Error(e.to_string()),
    }
}

// ── Private setup phases ─────────────────────────────────────────────────────

/// Open the project-local memory database if memory is enabled.
///
/// Returns `None` when `[memory] enabled` is `false` or the database
/// fails to open (in which case a warning is printed to stderr).
fn open_memory(sandbox: &SandboxRoot, config: &RhoConfig) -> Option<Arc<Memory>> {
    if !config.memory.enabled {
        return None;
    }

    let db_path = sandbox.path().join(".rho").join("memory.db");
    match tokio::runtime::Handle::current().block_on(Memory::open(&db_path)) {
        Ok(mem) => {
            tracing::info!(path = %db_path.display(), "memory database opened");
            Some(Arc::new(mem))
        }
        Err(e) => {
            P::config_warning(&format!(
                "failed to open memory database at {}: {e}",
                db_path.display()
            ));
            None
        }
    }
}

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
fn build_token_budget(
    config: &RhoConfig,
    cli: &Cli,
    catalog_model: Option<&rho_ai::Model>,
) -> TokenBudget {
    // Catalog-derived context window takes priority when available.
    // Otherwise, use CLI override, then config.
    let context_window = catalog_model.map_or_else(
        || cli.token_budget.unwrap_or(config.agent.token_budget) as usize,
        |m| usize::try_from(m.context_window).unwrap_or(usize::MAX),
    );

    // Cap completion_reserve at the model's max output tokens.
    let configured_reserve = config.agent.completion_reserve as usize;
    let completion_reserve = catalog_model.map_or_else(
        || configured_reserve,
        |m| configured_reserve.min(usize::try_from(m.max_tokens).unwrap_or(usize::MAX)),
    );

    TokenBudget::with_reserve(context_window, completion_reserve)
}

/// Construct the session.
///
/// Delegates to the appropriate construction path based on [`SessionMode`].
fn build_session(config: SessionConfig) -> Result<Session> {
    let SessionConfig {
        model,
        system_prompt,
        tool_schemas,
        sandbox,
        token_budget,
        redactor,
        reasoning_effort,
        session_path_holder,
        mode,
    } = config;

    match mode {
        SessionMode::Continue => {
            let path = rho_core::find_latest_session(sandbox.path())
                .ok_or_else(|| anyhow::anyhow!("no previous sessions found for this project"))?;
            P::session_resumed(&path);
            resume_session(
                &path,
                &model,
                &tool_schemas,
                &sandbox,
                token_budget,
                redactor,
                &session_path_holder,
            )
        }
        SessionMode::Resume(path) => {
            P::session_resumed(&path);
            resume_session(
                &path,
                &model,
                &tool_schemas,
                &sandbox,
                token_budget,
                redactor,
                &session_path_holder,
            )
        }
        SessionMode::Ephemeral => {
            let mut s =
                Session::in_memory(&model, Some(&system_prompt), tool_schemas, sandbox.path())
                    .with_token_budget(token_budget)
                    .with_redactor(redactor);
            if let Some(ref effort) = reasoning_effort {
                s = s.with_reasoning_effort(effort);
            }
            Ok(s)
        }
        SessionMode::Persisted => {
            let mut s = Session::new(&model, Some(&system_prompt), tool_schemas, sandbox.path())
                .with_token_budget(token_budget)
                .with_redactor(redactor);
            if let Some(ref effort) = reasoning_effort {
                s = s.with_reasoning_effort(effort);
            }
            if let Some(path) = s.save_path() {
                P::session_created(path);
                rho_tools::SessionSummary::set_path(&session_path_holder, path.to_path_buf());
            }
            let previous = rho_core::list_sessions(sandbox.path());
            if !previous.is_empty() {
                P::previous_sessions_hint(previous.len());
            }
            Ok(s)
        }
    }
}

/// Resume a session from a JSONL file with stale-CWD detection.
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
fn resolve_model(config: &RhoConfig, cli_model: Option<&String>) -> crate::model::ResolvedModel {
    crate::model::resolve_model(config, cli_model)
}

/// Ensure an `OpenRouter` provider exists in the registry and return its index.
///
/// The built-in default model (`anthropic/claude-sonnet-4`) is an `OpenRouter`
/// model id. When the zero-config fallback selects it, the only provider in
/// the registry is the `localhost:1234` default, which cannot serve it — so
/// the first request fails with a connection error.
///
/// This synthesizes an `OpenRouter` provider from the `openrouter` preset
/// (picking up `OPENROUTER_API_KEY` from the environment) so the default
/// model is actually reachable. If an `openrouter` provider is already
/// configured, it is reused unchanged.
///
/// Because the synthesized provider is external, the subsequent provider
/// consent check gates it normally: headless callers must pass
/// `--accept-external-provider` (rho-code does), and bare `rho` gets a clear
/// consent error instead of a silent runtime failure.
fn ensure_openrouter_provider(registry: &mut ProviderRegistry) -> usize {
    const OPENROUTER_PROVIDER_NAME: &str = "openrouter";

    if let Some(index) = registry.index_of(OPENROUTER_PROVIDER_NAME) {
        return index;
    }

    let Some(endpoint) = rho_core::config::preset_endpoint(OPENROUTER_PROVIDER_NAME) else {
        // Should never happen: "openrouter" is a built-in preset. Fall back
        // to the default provider rather than crashing startup.
        tracing::error!("openrouter preset not found; cannot synthesize default provider");
        return 0;
    };

    let api_key = rho_core::config::preset_api_key_env(OPENROUTER_PROVIDER_NAME)
        .and_then(|var| std::env::var(var).ok());
    let provider =
        OpenAiCompatibleProvider::new(OPENROUTER_PROVIDER_NAME, endpoint.to_owned(), api_key);
    registry.add(Box::new(provider));
    let index = registry.providers().len() - 1;
    tracing::info!(
        provider = OPENROUTER_PROVIDER_NAME,
        endpoint = endpoint,
        index,
        "synthesized OpenRouter provider for built-in default model"
    );
    index
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
        let result = rho_core::AgentResult {
            reply: "ok".into(),
            iterations: 1,
            usage: rho_core::TokenUsage::default(),
            tool_calls: vec![],
            duration: std::time::Duration::ZERO,
            finish_reason: rho_core::LoopFinishReason::Stop,
            context_stats: rho_core::ContextStats::default(),
        };
        let done = TurnResult::Done(Box::new(result));
        assert!(matches!(done, TurnResult::Done(_)));

        let err = TurnResult::Error("fail".into());
        assert!(matches!(err, TurnResult::Error(_)));
        let TurnResult::Error(msg) = err else {
            panic!("expected Error variant");
        };
        assert_eq!(msg, "fail");
    }

    #[test]
    fn set_model_error_messages_name_the_offender() {
        // These strings are surfaced verbatim to the user as JSON-RPC error
        // messages, so they must identify what was rejected.
        let m = SetModelError::ModelNotFound {
            model: "z.ai/glm-5-turbo".into(),
        }
        .message();
        assert!(m.contains("z.ai/glm-5-turbo"));
        assert!(m.contains("/models"));

        let m = SetModelError::UnknownProvider {
            provider: "acme".into(),
            model: "acme-7b".into(),
        }
        .message();
        assert!(m.contains("acme"));
        assert!(m.contains("/providers"));
    }
}
