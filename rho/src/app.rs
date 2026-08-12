//! Application assembly and startup orchestration.
//!
//! [`App`] is a thin host shell around a [`rho_core::Agent`]: it owns the
//! agent plus the host-only concerns the kernel stays ignorant of — the
//! TypeScript extension loader/observers and the tracing guard. [`App::build`]
//! runs the host setup (sandbox, config, memory, tools, extensions, context
//! files, system prompt) and then maps the CLI into an [`rho_core::AgentBuilder`]
//! to assemble the kernel [`rho_core::Agent`].

use crate::cli::Cli;
use crate::presenter::RpcPresenter as P;
use anyhow::{Context, Result};
use rho_core::{
    Agent, ConfigLoader, Provider, RhoConfig, SandboxRoot, Session, SwitchModelError, ToolRegistry,
    compose_full_system_prompt,
    context_files::{ContextFile, ContextScanner, TrustStore},
    find_project_root,
};
use rho_ext::DenoObserver;
use rho_ext::loader::ExtensionLoader;
use rho_memory::Memory;
use rho_tools::register_all;
use std::io;
use std::sync::Arc;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

// ── App ───────────────────────────────────────────────────────────────────────

/// Long-lived runtime state for the `rho` agent.
///
/// A thin host shell: the kernel state lives in [`Agent`]; this struct adds
/// only the host-only extension runtime and the tracing guard. Constructed by
/// [`App::build`]; call [`App::run`] to start the RPC loop.
pub struct App {
    /// The kernel agent (session, providers, tools, config, cancel).
    pub(crate) agent: Agent,
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
    /// Runs the host setup (tracing, sandbox, config, memory, tools,
    /// extensions, context files, system prompt) and then maps the CLI into an
    /// [`AgentBuilder`] to assemble the kernel [`Agent`]. Host-only concerns
    /// (extensions, tracing, consent diagnostics, banners) stay here; the
    /// kernel owns providers/model/budget/session.
    ///
    /// # Errors
    ///
    /// Returns an error if any setup phase fails (bad sandbox path, external
    /// provider consent required, malformed session file).
    #[allow(clippy::too_many_lines)]
    pub async fn build(cli: Cli) -> Result<Self> {
        // ── Host: tracing ───────────────────────────────────────────────
        let _log_guard = Self::init_tracing();

        // ── Host: sandbox + config ──────────────────────────────────────
        let sandbox = resolve_sandbox(&cli)?;
        let config = load_config(&sandbox);

        // ── Host: memory + built-in tools + extensions ──────────────────
        let mut tool_registry = ToolRegistry::new();
        let memory = open_memory(&sandbox, &config).await;
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

        // ── Host: context files (headless: auto-deny untrusted) + prompt ──
        let context_files = scan_context_files(&sandbox);
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

        // ── Map CLI → AgentBuilder; build the kernel Agent ───────────────
        let mut builder = Agent::builder()
            .config(config)
            .sandbox(sandbox)
            .system_prompt(system_prompt)
            .tools(tool_registry);
        if let Some(model) = cli.model.as_deref() {
            builder = builder.model(model);
        }
        if let Some(endpoint) = cli.endpoint.as_deref() {
            builder = builder.endpoint(endpoint);
        }
        if let Some(var) = cli.api_key_env.as_deref() {
            builder = builder.api_key_env(var);
        }
        if let Some(n) = cli.max_iterations {
            builder = builder.max_iterations(n);
        }
        if let Some(tb) = cli.token_budget {
            builder = builder.context_window(tb);
        }
        builder = match (cli.r#continue, &cli.session, cli.ephemeral) {
            (true, _, _) => builder.continue_last(),
            (_, Some(path), _) => builder.resume(path),
            (_, _, true) => builder.ephemeral(),
            _ => builder.persisted(),
        };
        if cli.accept_external_provider || cli.endpoint.is_some() {
            builder = builder.accept_external_consent();
        }

        let agent = builder.build().map_err(|e| anyhow::anyhow!("{e}"))?;

        // ── Host: post-build wiring (banners + extension model propagation) ──
        let resumed = cli.r#continue || cli.session.is_some();
        if resumed {
            if let Some(path) = agent.session().save_path() {
                P::session_resumed(path);
            }
        } else if let Some(path) = agent.session().save_path() {
            // Persisted mode (ephemeral has no save_path). Link the
            // SessionSummary tool to the new file and hint at prior sessions.
            P::session_created(path);
            rho_tools::SessionSummary::set_path(&session_path_holder, path.to_path_buf());
            let previous = rho_core::list_sessions(&agent.session().header().cwd);
            if !previous.is_empty() {
                P::previous_sessions_hint(previous.len());
            }
        }
        // Propagate the resolved model id to extensions (rho.getModel()).
        ext_loader.set_model_all(agent.session().model()).await;
        // Budget diagnostics.
        log_budget_diagnostics(agent.session());

        Ok(Self {
            agent,
            ext_loader,
            ext_observers,
            #[allow(clippy::used_underscore_binding)]
            _log_guard,
        })
    }

    /// The currently active provider.
    #[must_use]
    pub fn active_provider(&self) -> &dyn Provider {
        self.agent.active_provider()
    }

    /// Start a fresh session, preserving model/provider, tools, token budget,
    /// redactor, reasoning effort, system prompt, and user-defined models.
    ///
    /// Returns the new `(session_id, path)`; `path` is empty for in-memory
    /// sessions.
    pub(crate) fn start_new_session(&mut self) -> (String, String) {
        self.agent.new_session()
    }

    /// Switch the active model at runtime, optionally targeting a provider via
    /// `provider:model` syntax. Delegates the core switch to the agent and
    /// propagates the new model id to all loaded extensions.
    ///
    /// # Errors
    ///
    /// Returns [`SwitchModelError::ModelNotFound`] if a bare id is not
    /// advertised by any provider, or [`SwitchModelError::UnknownProvider`] if
    /// the `provider:` prefix names a provider that isn't configured.
    pub(crate) async fn set_model(&mut self, spec: &str) -> Result<(), SwitchModelError> {
        self.agent.switch_model(spec).await?;
        self.ext_loader
            .set_model_all(self.agent.session().model())
            .await;
        Ok(())
    }

    /// Trigger context compaction on the active session (mechanical strategy,
    /// threshold = one-quarter of the message budget).
    ///
    /// # Errors
    ///
    /// Propagates [`rho_core::RhoError`] if compaction fails.
    pub(crate) async fn compact(&mut self) -> rho_core::Result<()> {
        self.agent.compact().await
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

// ── Private host setup helpers ──────────────────────────────────────────────

/// Open the project-local memory database if memory is enabled.
///
/// Returns `None` when `[memory] enabled` is `false` or the database
/// fails to open (in which case a warning is printed to stderr).
async fn open_memory(sandbox: &SandboxRoot, config: &RhoConfig) -> Option<Arc<Memory>> {
    if !config.memory.enabled {
        return None;
    }

    let db_path = sandbox.path().join(".rho").join("memory.db");
    match Memory::open(&db_path).await {
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
