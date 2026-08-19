//! Composable agent orchestration: the [`Agent`] builder.
//!
//! [`Agent`] owns the long-lived agent state — the conversation [`Session`],
//! the [`ProviderRegistry`], the [`ToolRegistry`], the loop [`AgentConfig`],
//! and a cooperative [`CancellationToken`] — and exposes a clean programmatic
//! API (`run` / `run_turn` / `switch_model` / ...) so every consumer (the `rho`
//! binary's RPC loop, embedders, tests, benches) is a thin wrapper.
//! Construction is decoupled from CLI flags, config files, and stdio via the
//! fluent [`AgentBuilder`].
//!
//! ## Separation principle
//!
//! Host-only concerns stay *out* of `rho-core`: stdio RPC, the V8 extension
//! runtime, the tracing subscriber, interactive consent prompts, and startup
//! banners are composed on top by the host. Per-turn, per-connection inputs
//! (the UI observer, the approval gate, the steering source) are supplied via
//! [`TurnInputs`] when calling [`Agent::run_turn`]; [`Agent::run`] supplies
//! headless defaults (a no-op observer + an always-approve gate, no steering).

use crate::config::{UserModelConfig, preset_api_key_env, preset_endpoint};
use crate::message::ModelToolCall;
use crate::tool::{CancellationToken, ToolRisk};
use crate::{
    AgentConfig, AgentObserver, AgentResult, ApprovalDecision, ApprovalGate, ContextStats,
    LoopParams, MechanicalCompactionStrategy, ModelInfo, NopObserver, OpenAiCompatibleProvider,
    Provider, ProviderRegistry, Redactor, RhoConfig, SandboxRoot, Session, SteeringSource,
    TokenBudget, ToolDefinition, ToolRegistry, compose_full_system_prompt, find_latest_session,
    format_suggestions, fuzzy_match, run_loop,
};
use async_trait::async_trait;
use rho_ai::catalog::{Catalog, Model};
use std::path::{Path, PathBuf};

// ── Per-turn inputs ─────────────────────────────────────────────────────────

/// Per-turn, per-connection inputs supplied to [`Agent::run_turn`].
///
/// These are deliberately *not* owned by [`Agent`]: an observer/approval-gate
/// is tied to a UI connection (e.g. an RPC transport) and may change between
/// turns (e.g. when extensions reload); the cancellation token is supplied
/// per turn so the caller can swap a fresh one in (the RPC layer does this so
/// a prior `abort` doesn't poison the next turn). The caller composes them and
/// passes them in for each turn. [`Agent::run`] supplies headless defaults.
pub struct TurnInputs<'a> {
    /// UI callback for state changes and streaming deltas.
    pub observer: &'a dyn AgentObserver,
    /// Approval gate (prompts the user to approve tool calls).
    pub gate: &'a dyn ApprovalGate,
    /// Optional source of mid-turn steering messages, drained between
    /// tool-batch completion and the next thinking step (see [`SteeringSource`]).
    pub steering: Option<&'a dyn SteeringSource>,
    /// Cooperative cancellation token for this turn. The caller owns the
    /// cancellation source (the RPC layer swaps a fresh token in per turn);
    /// this clone feeds the agent loop. [`Agent::run`] clones the agent's token.
    pub cancel: CancellationToken,
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Why a runtime model switch was rejected.
///
/// A rejected switch leaves the session's active model and provider untouched,
/// so the next prompt continues with the previously working model instead of
/// failing at request time with an opaque HTTP error.
#[derive(Debug)]
pub enum SwitchModelError {
    /// A bare model id was not advertised by any provider's `/v1/models`.
    ModelNotFound {
        /// The rejected model identifier.
        model: String,
        /// Formatted fuzzy-match suggestions from the providers' advertised
        /// models ("  qwen3-8b (provider: local)" lines), empty when no
        /// candidate clears the similarity threshold.
        suggestions: String,
    },
    /// `provider:model` syntax named a provider that isn't configured.
    UnknownProvider {
        /// The rejected provider name.
        provider: String,
        /// The model identifier that was to be sent to it.
        model: String,
    },
}

impl SwitchModelError {
    /// Human-readable message suitable for surfacing to the user.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::ModelNotFound { model, suggestions } => {
                let mut msg = format!(
                    "model '{model}' was not found on any provider; use /models to list \
                     available models, or 'provider:{model}' to target a provider that \
                     does not advertise models via /v1/models"
                );
                if !suggestions.is_empty() {
                    msg.push_str("\nDid you mean:\n");
                    msg.push_str(suggestions);
                }
                msg
            }
            Self::UnknownProvider { provider, model } => format!(
                "unknown provider '{provider}' for model '{model}'; use /providers to \
                 list configured providers"
            ),
        }
    }
}

/// Why [`AgentBuilder::build`] failed.
#[derive(Debug)]
pub enum AgentBuildError {
    /// The sandbox root could not be established.
    Sandbox(String),
    /// External providers are configured but consent was not granted.
    ExternalConsentRequired {
        /// Names of the external providers that require consent.
        providers: Vec<String>,
    },
    /// A session could not be constructed, opened, or resumed.
    Session(String),
}

impl std::fmt::Display for AgentBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sandbox(msg) => write!(f, "sandbox root: {msg}"),
            Self::ExternalConsentRequired { providers } => write!(
                f,
                "external provider consent required for [{}]; grant via \
                 `.accept_external_consent()` or disable the check with \
                 `.check_external_consent(false)`",
                providers.join(", ")
            ),
            Self::Session(msg) => write!(f, "session: {msg}"),
        }
    }
}

impl std::error::Error for AgentBuildError {}

// ── Agent ───────────────────────────────────────────────────────────────────

/// An assembled, ready-to-run agent.
///
/// Owns the long-lived runtime state: the conversation [`Session`], the
/// [`ProviderRegistry`], the [`ToolRegistry`], the loop [`AgentConfig`], and a
/// cooperative [`CancellationToken`]. Construct it via [`Agent::builder`].
///
/// Host-only concerns (extensions, tracing, the RPC transport) live *outside*
/// this type and are supplied per-turn through [`TurnInputs`].
pub struct Agent {
    /// The conversation session (tree-shaped, optionally persisted).
    pub(crate) session: Session,
    /// Configured providers and the index of the active one.
    pub(crate) providers: ProviderRegistry,
    /// Index of the active provider in [`Agent::providers`].
    pub(crate) active_provider_index: usize,
    /// Registered tools.
    pub(crate) registry: ToolRegistry,
    /// Agent loop configuration (max iterations, retry budget, compaction, ...).
    pub(crate) config: AgentConfig,
    /// Cooperative cancellation token.
    pub(crate) cancel: CancellationToken,
}

impl Agent {
    /// Start an [`AgentBuilder`].
    #[must_use]
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    /// Run one turn, injecting per-turn observer/gate/steering.
    ///
    /// Frontends call this with their own [`TurnInputs`]; headless/embed
    /// callers use [`Agent::run`] instead.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::RhoError`] from the agent loop (model/transport
    /// failures, tool errors, cancellation, iteration limit).
    pub async fn run_turn(
        &mut self,
        prompt: &str,
        inputs: &TurnInputs<'_>,
    ) -> crate::Result<AgentResult> {
        // Clone the LLM service off the active provider so the loop does not
        // borrow `self` while it also borrows `&mut self.session` (disjoint
        // field borrows below). Mirrors `rho/src/rpc.rs`'s LoopParams assembly.
        let client = self.active_provider().clone_boxed_service();
        let compaction_client = if self.config.compaction_mode == "llm" {
            Some(std::sync::Arc::<dyn rho_ai::LlmService>::from(
                self.active_provider().clone_boxed_service(),
            ))
        } else {
            None
        };
        let params = LoopParams {
            client: client.as_ref(),
            registry: &self.registry,
            config: &self.config,
            cancel: inputs.cancel.clone(),
            gate: inputs.gate,
            observer: inputs.observer,
            compaction_client,
            steering: inputs.steering,
        };
        run_loop(&mut self.session, prompt, &params).await
    }

    /// Run one turn with headless defaults: a no-op observer, an
    /// always-approve gate, and no steering. Returns the structured
    /// [`AgentResult`].
    ///
    /// **The default gate auto-approves every tool call** so an embedder's turn
    /// runs to completion without prompting. This is appropriate for local,
    /// trusted, interactive embedders and tests; for any unattended/daemon use,
    /// call [`Agent::run_turn`] with an interactive [`ApprovalGate`] instead.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::RhoError`] from the agent loop.
    pub async fn run(&mut self, prompt: &str) -> crate::Result<AgentResult> {
        let observer = NopObserver;
        let gate = AlwaysApproveGate;
        self.run_turn(
            prompt,
            &TurnInputs {
                observer: &observer,
                gate: &gate,
                steering: None,
                cancel: self.cancel.clone(),
            },
        )
        .await
    }

    /// Switch the active model at runtime, optionally targeting a provider via
    /// `provider:model` syntax.
    ///
    /// On success, updates the session's model, recomputes the token budget
    /// from the catalog, and switches the active provider. (Propagating the new
    /// model id to extensions is the host's responsibility — [`Agent`] is
    /// extension-agnostic.)
    ///
    /// # Errors
    ///
    /// Returns [`SwitchModelError::ModelNotFound`] if a bare id is not
    /// advertised by any provider, or [`SwitchModelError::UnknownProvider`] if
    /// the `provider:` prefix names a provider that isn't configured.
    pub async fn switch_model(&mut self, spec: &str) -> Result<(), SwitchModelError> {
        let (explicit_provider, model_id) = spec
            .split_once(':')
            .map_or((None, spec), |(p, m)| (Some(p), m));

        if let Some(provider_name) = explicit_provider {
            if let Some(index) = self.providers.index_of(provider_name) {
                self.active_provider_index = index;
            } else {
                return Err(SwitchModelError::UnknownProvider {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                });
            }
        } else if let Some(index) = self.providers.find_model_index(model_id).await {
            self.active_provider_index = index;
        } else {
            // The probe already fetched every provider's advertised models;
            // reuse that data to suggest close matches instead of making the
            // user re-run /models themselves.
            let available: Vec<(&str, String)> = self
                .providers
                .list_all_models()
                .await
                .into_iter()
                .map(|(name, info)| (name, info.id().to_owned()))
                .collect();
            let suggestions = format_suggestions(&fuzzy_match(model_id, &available, 0.5), 5);
            return Err(SwitchModelError::ModelNotFound {
                model: model_id.to_owned(),
                suggestions,
            });
        }

        // Re-compute the token budget from the catalog so a smaller context
        // window takes effect immediately rather than keeping the old budget.
        let catalog_model = Catalog::resolve(None, model_id);
        let prev = self.session.token_budget();
        let new_budget =
            budget_for_model(catalog_model, prev.context_window, prev.completion_reserve);
        self.session.set_token_budget(new_budget);
        self.session.set_model(model_id);
        Ok(())
    }

    /// Start a fresh session, preserving model, provider, tools, token budget,
    /// redactor, reasoning effort, system prompt, and user-defined models.
    ///
    /// Returns `(session_id, path)`; `path` is empty for in-memory sessions.
    pub fn new_session(&mut self) -> (String, String) {
        let model = self.session.model().to_owned();
        let cwd = self.session.header().cwd.clone();
        let budget = self.session.token_budget();
        let redactor = self.session.redactor().clone();
        let reasoning = self.session.reasoning_effort.clone();
        let tool_schemas = self.registry.tool_definitions();
        let system_prompt = self.session.system_prompt().map(str::to_owned);
        let user_models = self.session.user_models.clone();

        let mut session = if self.session.save_path().is_some() {
            Session::new(&model, system_prompt.as_deref(), tool_schemas, &cwd)
        } else {
            Session::in_memory(&model, system_prompt.as_deref(), tool_schemas, &cwd)
        }
        .with_token_budget(budget)
        .with_redactor(redactor)
        .with_user_models(user_models);
        if let Some(effort) = reasoning {
            session = session.with_reasoning_effort(&effort);
        }

        let path = session
            .save_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session_id = session.header().id.to_string();
        self.session = session;
        (session_id, path)
    }

    /// Trigger context compaction on the active session (mechanical strategy,
    /// threshold = one-quarter of the message budget).
    ///
    /// # Errors
    ///
    /// Propagates [`crate::RhoError`] if compaction fails.
    pub async fn compact(&mut self) -> crate::Result<()> {
        let strategy = MechanicalCompactionStrategy::new();
        let threshold = self.session.message_budget() / 4;
        self.session
            .compact_older_than(threshold, &strategy)
            .await
            .map(|_| ())
    }

    /// List all models across every provider (queries `/v1/models`).
    pub async fn list_models(&self) -> Vec<(&str, ModelInfo)> {
        self.providers.list_all_models().await
    }

    /// The currently active provider.
    #[must_use]
    pub fn active_provider(&self) -> &dyn Provider {
        self.providers.providers()[self.active_provider_index].as_ref()
    }

    /// The conversation session (escape hatch for advanced callers).
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Mutable access to the session (escape hatch).
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// The cooperative cancellation token (share clones to cancel from outside).
    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// The provider registry (for model/provider listing and lookups).
    #[must_use]
    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// The tool registry (for inspecting registered tool definitions).
    #[must_use]
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Mutable access to the tool registry (for runtime tool/extension reload).
    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Registered tool definitions.
    #[must_use]
    pub fn list_tools(&self) -> &[ToolDefinition] {
        self.session.tools()
    }

    /// Context-window stats for the active session.
    #[must_use]
    pub fn context_stats(&self) -> ContextStats {
        self.session.context_stats()
    }
}

/// Headless default approval gate: approves every tool call without prompting.
///
/// Used only by [`Agent::run`]. Production/front-end code supplies its own
/// [`ApprovalGate`] via [`TurnInputs`].
struct AlwaysApproveGate;

#[async_trait]
impl ApprovalGate for AlwaysApproveGate {
    async fn request_approval(&self, _call: &ModelToolCall, _risk: ToolRisk) -> ApprovalDecision {
        ApprovalDecision::Approved
    }
}

// ── AgentBuilder ────────────────────────────────────────────────────────────

/// Fluent builder for [`Agent`].
///
/// Pick a construction source (`.config(...)` for a programmatic config, or
/// `.load_config_at(...)` to read TOML), then override individual fields. No
/// `Cli` is required — the `rho` binary maps its CLI into these setters.
///
/// ```ignore
/// let agent = Agent::builder()
///     .config(cfg)
///     .ephemeral()
///     .model("gpt-4o")
///     .build()?;
/// ```
pub struct AgentBuilder {
    /// Programmatic config (overrides `load_config_at`).
    config: Option<RhoConfig>,
    /// Sandbox root (defaults to the current directory).
    sandbox: Option<SandboxRoot>,
    /// Model id override (highest priority).
    model: Option<String>,
    /// Endpoint URL override.
    endpoint: Option<String>,
    /// API-key env-var override.
    api_key_env: Option<String>,
    /// Pre-composed system prompt (host responsibility; defaults to the base prompt).
    system_prompt: Option<String>,
    /// Pre-built tool registry (host registers tools/extensions/memory).
    tools: Option<ToolRegistry>,
    /// Injected provider registry (test/embed path; otherwise built from config).
    providers: Option<ProviderRegistry>,
    /// Token-budget override.
    token_budget: Option<TokenBudget>,
    /// Context-window override, used as the fallback when no catalog model is
    /// known (mirrors the `--token-budget` flag).
    context_window: Option<u32>,
    /// Max-iterations override.
    max_iterations: Option<u32>,
    /// Session construction mode.
    session_mode: SessionMode,
    /// Whether to enforce the external-provider consent gate.
    check_consent: bool,
    /// Whether consent has been explicitly granted.
    consent_accepted: bool,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self {
            config: None,
            sandbox: None,
            model: None,
            endpoint: None,
            api_key_env: None,
            system_prompt: None,
            tools: None,
            providers: None,
            token_budget: None,
            context_window: None,
            max_iterations: None,
            session_mode: SessionMode::Ephemeral,
            check_consent: true,
            consent_accepted: false,
        }
    }
}

// Fluent builder: every setter returns `Self` for chaining.
#[allow(clippy::return_self_not_must_use)]
impl AgentBuilder {
    /// Create a new builder with defaults (ephemeral session, consent check on).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supply a programmatic config (takes precedence over [`Self::load_config_at`]).
    pub fn config(mut self, config: RhoConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Load config from TOML at the given sandbox path.
    pub fn load_config_at(mut self, sandbox: &Path) -> Self {
        self.config = Some(crate::ConfigLoader::load(sandbox).unwrap_or_default());
        self
    }

    /// Set the sandbox root (defaults to the current directory).
    pub fn sandbox(mut self, sandbox: SandboxRoot) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Override the model id (highest resolution priority).
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Override the provider endpoint URL.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Override the API-key env var.
    pub fn api_key_env(mut self, var: impl Into<String>) -> Self {
        self.api_key_env = Some(var.into());
        self
    }

    /// Supply a pre-composed system prompt (skips default base-prompt composition).
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Supply a pre-built tool registry (the host registers tools/extensions/memory).
    pub fn tools(mut self, registry: ToolRegistry) -> Self {
        self.tools = Some(registry);
        self
    }

    /// Inject a provider registry (test/embed path); otherwise built from config.
    pub fn providers(mut self, registry: ProviderRegistry) -> Self {
        self.providers = Some(registry);
        self
    }

    /// Override the token budget.
    pub fn token_budget(mut self, budget: TokenBudget) -> Self {
        self.token_budget = Some(budget);
        self
    }

    /// Override the context window (used as the fallback when the model is not
    /// in the built-in catalog; mirrors the `--token-budget` flag).
    pub fn context_window(mut self, context_window: u32) -> Self {
        self.context_window = Some(context_window);
        self
    }

    /// Override the max agent-loop iterations.
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = Some(n);
        self
    }

    /// Use an in-memory session (no disk I/O). This is the default.
    pub fn ephemeral(mut self) -> Self {
        self.session_mode = SessionMode::Ephemeral;
        self
    }

    /// Use a new persisted session at `~/.rho/sessions/<project-hash>/`.
    pub fn persisted(mut self) -> Self {
        self.session_mode = SessionMode::Persisted;
        self
    }

    /// Resume the most recent session for this project.
    pub fn continue_last(mut self) -> Self {
        self.session_mode = SessionMode::Continue;
        self
    }

    /// Resume a specific session JSONL file.
    pub fn resume(mut self, path: impl Into<PathBuf>) -> Self {
        self.session_mode = SessionMode::Resume(path.into());
        self
    }

    /// Toggle the external-provider consent gate (on by default).
    pub fn check_external_consent(mut self, on: bool) -> Self {
        self.check_consent = on;
        self
    }

    /// Explicitly grant external-provider consent (the binary does this when
    /// `--accept-external-provider` or `--endpoint` is passed).
    pub fn accept_external_consent(mut self) -> Self {
        self.consent_accepted = true;
        self
    }

    /// Assemble the [`Agent`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentBuildError`] if the sandbox cannot be established,
    /// external-provider consent is required but not granted, or the session
    /// cannot be constructed/resumed.
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        // Sandbox (default: current directory).
        let sandbox = match self.sandbox {
            Some(s) => s,
            None => SandboxRoot::new(std::env::current_dir().map_err(|e| {
                AgentBuildError::Sandbox(format!("cannot determine current directory: {e}"))
            })?)
            .map_err(|e| AgentBuildError::Sandbox(e.to_string()))?,
        };

        // Config (programmatic > loaded > default).
        let config = self.config.unwrap_or_default();

        // Providers (injected > from config).
        let mut providers = self.providers.unwrap_or_else(|| {
            ProviderRegistry::from_config(
                &config.provider,
                self.endpoint.as_deref(),
                self.api_key_env.as_deref(),
            )
        });

        // Model resolution + default-provider synthesis.
        let resolved = resolve_model(&config, self.model.as_deref());
        let active_provider_index = if resolved.is_builtin_default {
            ensure_openrouter_provider(&mut providers)
        } else {
            resolved
                .provider
                .as_deref()
                .and_then(|name| providers.index_of(name))
                .unwrap_or(0)
        };
        let model = resolved.id.clone();

        // Informational: warn on non-OpenAI-shaped provider types.
        warn_non_openai_types(&config);

        // Consent gate (host opts in via `accept_external_consent`).
        if self.check_consent && !self.consent_accepted {
            let external: Vec<String> = providers
                .providers()
                .iter()
                .filter(|p| p.is_external())
                .map(|p| p.name().to_owned())
                .collect();
            if !external.is_empty() {
                return Err(AgentBuildError::ExternalConsentRequired {
                    providers: external,
                });
            }
        }

        // Tools (host-provided; default empty).
        let registry = self.tools.unwrap_or_default();
        let tool_schemas = registry.tool_definitions();

        // Token budget.
        let token_budget = self.token_budget.unwrap_or_else(|| {
            resolve_budget(
                &config,
                resolved.catalog_model.as_ref(),
                self.context_window,
            )
        });

        // Agent config (with max-iterations override).
        let mut agent_config = AgentConfig::from_config(&config);
        if let Some(n) = self.max_iterations {
            agent_config.max_iterations = n;
        }

        // Redactor + reasoning effort.
        let redactor =
            Redactor::from_config(config.redaction.enabled, &config.redaction.custom_patterns);
        let reasoning_effort = resolve_reasoning_effort(
            resolved.catalog_model.as_ref(),
            config.agent.reasoning_effort.as_deref(),
        );

        // System prompt (host-provided, else the base prompt with no context).
        let system_prompt = self
            .system_prompt
            .unwrap_or_else(|| compose_full_system_prompt(&sandbox, &[], &config, None, false));

        // Session.
        let mut session = build_session(
            &model,
            &system_prompt,
            &tool_schemas,
            &sandbox,
            token_budget,
            redactor,
            reasoning_effort.as_deref(),
            self.session_mode,
        )?;
        session.set_user_models(
            config
                .models
                .iter()
                .map(UserModelConfig::to_catalog_model)
                .collect(),
        );

        Ok(Agent {
            session,
            providers,
            active_provider_index,
            registry,
            config: agent_config,
            cancel: CancellationToken::new(),
        })
    }
}

// ── Private helpers (ported from rho/src/app.rs, minus Cli/presenter) ────────

/// Session construction mode.
enum SessionMode {
    /// In-memory session, no disk I/O (the builder default).
    Ephemeral,
    /// New persisted session.
    Persisted,
    /// Resume the most recent session for the project.
    Continue,
    /// Resume a specific JSONL file.
    Resume(PathBuf),
}

/// Resolved model identifier with optional catalog enrichment.
struct ResolvedModel {
    /// The raw model id string.
    id: String,
    /// Which configured provider handles it, if known.
    provider: Option<String>,
    /// Catalog entry if the model is known to the built-in catalog.
    catalog_model: Option<Model>,
    /// `true` only when the built-in default fallback supplied this model.
    is_builtin_default: bool,
}

/// Resolve the model id without network calls.
///
/// Priority (first match wins): explicit override → `agent.model` →
/// `agent.provider` + that provider's `default_model` → first provider's
/// `default_model` → `RHO_MODEL` env → built-in default.
fn resolve_model(config: &RhoConfig, model_override: Option<&str>) -> ResolvedModel {
    // 1. Explicit override (CLI `--model` / builder `.model()`).
    if let Some(model) = model_override {
        let catalog_model = Catalog::resolve(None, model).cloned();
        return ResolvedModel {
            id: model.to_owned(),
            provider: None,
            catalog_model,
            is_builtin_default: false,
        };
    }

    // 2. Config agent.model.
    if let Some(model) = config.agent.model.as_deref() {
        let provider = provider_for_model(model, config);
        let catalog_model = Catalog::resolve(None, model).cloned();
        return ResolvedModel {
            id: model.to_owned(),
            provider,
            catalog_model,
            is_builtin_default: false,
        };
    }

    // 3. Config agent.provider + that provider's default_model.
    if let Some(provider_name) = config.agent.provider.as_deref()
        && let Some(provider_config) = config
            .provider
            .providers
            .iter()
            .find(|p| p.name.as_deref() == Some(provider_name))
        && let Some(model) = provider_config.default_model.as_deref()
    {
        let catalog_model = Catalog::resolve(None, model).cloned();
        return ResolvedModel {
            id: model.to_owned(),
            provider: Some(provider_name.to_owned()),
            catalog_model,
            is_builtin_default: false,
        };
    }

    // 4. First provider's default_model.
    if let Some(default) = config.provider.default_model() {
        let catalog_model = Catalog::resolve(None, default).cloned();
        return ResolvedModel {
            id: default.to_owned(),
            provider: config
                .provider
                .default_provider()
                .and_then(|p| p.name.clone()),
            catalog_model,
            is_builtin_default: false,
        };
    }

    // 5. RHO_MODEL env var.
    if let Ok(env_model) = std::env::var("RHO_MODEL") {
        let catalog_model = Catalog::resolve(None, &env_model).cloned();
        return ResolvedModel {
            id: env_model,
            provider: None,
            catalog_model,
            is_builtin_default: false,
        };
    }

    // 6. Built-in default (an OpenRouter id; caller synthesizes the provider).
    let default_id = rho_ai::catalog::DEFAULT_MODEL_ID;
    let catalog_model = Catalog::resolve(None, default_id).cloned();
    ResolvedModel {
        id: default_id.to_owned(),
        provider: None,
        catalog_model,
        is_builtin_default: true,
    }
}

/// Find which configured provider claims a model via its `default_model`.
fn provider_for_model(model: &str, config: &RhoConfig) -> Option<String> {
    config
        .provider
        .providers
        .iter()
        .find(|p| p.default_model.as_deref() == Some(model))
        .and_then(|p| p.name.clone())
}

/// Ensure an `openrouter` provider exists and return its index.
///
/// The built-in default model is an `OpenRouter` id that the zero-config
/// `localhost:1234` provider cannot serve, so a matching `OpenRouter` provider
/// is synthesized from the preset (picking up `OPENROUTER_API_KEY` from env).
fn ensure_openrouter_provider(registry: &mut ProviderRegistry) -> usize {
    const OPENROUTER: &str = "openrouter";
    if let Some(index) = registry.index_of(OPENROUTER) {
        return index;
    }
    let Some(endpoint) = preset_endpoint(OPENROUTER) else {
        return 0;
    };
    let api_key = preset_api_key_env(OPENROUTER).and_then(|var| std::env::var(var).ok());
    let provider = OpenAiCompatibleProvider::new(OPENROUTER, endpoint.to_owned(), api_key);
    registry.add(Box::new(provider));
    registry.providers().len() - 1
}

/// Resolve the token budget from config + optional catalog enrichment.
fn resolve_budget(
    config: &RhoConfig,
    catalog_model: Option<&Model>,
    ctx_override: Option<u32>,
) -> TokenBudget {
    let context_window = catalog_model.map_or_else(
        || ctx_override.unwrap_or(config.agent.token_budget) as usize,
        |m| usize::try_from(m.context_window).unwrap_or(usize::MAX),
    );
    let configured_reserve = config.agent.completion_reserve as usize;
    let completion_reserve = catalog_model.map_or_else(
        || configured_reserve,
        |m| configured_reserve.min(usize::try_from(m.max_tokens).unwrap_or(usize::MAX)),
    );
    TokenBudget::with_reserve(context_window, completion_reserve)
}

/// Recompute the token budget for a catalog model, keeping the current reserve
/// when the catalog has no opinion.
fn budget_for_model(
    catalog_model: Option<&Model>,
    current_context_window: usize,
    current_completion_reserve: usize,
) -> TokenBudget {
    let context_window = catalog_model.map_or(current_context_window, |m| {
        usize::try_from(m.context_window).unwrap_or(usize::MAX)
    });
    let completion_reserve = catalog_model.map_or_else(
        || current_completion_reserve,
        |m| current_completion_reserve.min(usize::try_from(m.max_tokens).unwrap_or(usize::MAX)),
    );
    TokenBudget::with_reserve(context_window, completion_reserve)
}

/// Resolve reasoning effort, suppressed when the model lacks thinking support.
fn resolve_reasoning_effort(
    catalog_model: Option<&Model>,
    config_effort: Option<&str>,
) -> Option<String> {
    match (catalog_model, config_effort) {
        (Some(catalog), Some(effort)) if catalog.thinking.supported => Some(effort.to_owned()),
        (Some(_), _) => None,
        (None, effort) => effort.map(str::to_owned),
    }
}

/// Warn (via tracing) on non-OpenAI-shaped provider types.
fn warn_non_openai_types(config: &RhoConfig) {
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
            tracing::warn!(
                provider_type,
                "provider type is not OpenAI-compatible; rho speaks Responses/Chat Completions only"
            );
        }
    }
}

/// Construct the session for the chosen mode, then apply the token budget,
/// redactor, and reasoning effort uniformly.
///
/// `Err` on a resume that finds no session or cannot open the file.
#[allow(clippy::too_many_arguments)] // private session-construction helper; args are cohesive
fn build_session(
    model: &str,
    system_prompt: &str,
    tools: &[ToolDefinition],
    sandbox: &SandboxRoot,
    budget: TokenBudget,
    redactor: Redactor,
    reasoning: Option<&str>,
    mode: SessionMode,
) -> Result<Session, AgentBuildError> {
    // `tools` is moved into exactly one (mutually exclusive) match arm.
    let tools = tools.to_vec();
    let mut session = match mode {
        SessionMode::Ephemeral => {
            Session::in_memory(model, Some(system_prompt), tools, sandbox.path())
        }
        SessionMode::Persisted => Session::new(model, Some(system_prompt), tools, sandbox.path()),
        SessionMode::Continue => {
            let path = find_latest_session(sandbox.path()).ok_or_else(|| {
                AgentBuildError::Session("no previous sessions found for this project".into())
            })?;
            resume_session(&path, model, tools, sandbox)?
        }
        SessionMode::Resume(path) => resume_session(&path, model, tools, sandbox)?,
    };
    session = session.with_token_budget(budget).with_redactor(redactor);
    if let Some(effort) = reasoning {
        session = session.with_reasoning_effort(effort);
    }
    Ok(session)
}

/// Resume a session from a JSONL file, applying the resolved model and tools.
///
/// Budget/redactor/reasoning are applied uniformly by [`build_session`] after
/// the mode-specific base session is constructed.
fn resume_session(
    path: &Path,
    model: &str,
    tools: Vec<ToolDefinition>,
    sandbox: &SandboxRoot,
) -> Result<Session, AgentBuildError> {
    let mut session = Session::open(path)
        .map_err(|e| AgentBuildError::Session(format!("failed to open session: {e}")))?;
    let session_cwd = session.header().cwd.clone();
    if session_cwd != sandbox.path() {
        tracing::warn!(
            session_cwd = ?session_cwd,
            current_cwd = ?sandbox.path(),
            "resuming a session whose stored cwd differs from the sandbox root"
        );
    }
    session.set_model(model);
    session.set_tools(tools);
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentLoopConfig, ProviderConfig, ProviderSettings};

    fn config_with(model: Option<&str>, providers: Vec<ProviderConfig>) -> RhoConfig {
        RhoConfig {
            agent: AgentLoopConfig {
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
            default_model: default_model.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_model_explicit_override_wins() {
        let config = config_with(
            Some("local-default"),
            vec![provider("local", Some("local-default"))],
        );
        let r = resolve_model(&config, Some("cli-model"));
        assert_eq!(r.id, "cli-model");
        assert!(!r.is_builtin_default);
    }

    #[test]
    fn resolve_model_config_model_resolves_provider() {
        let config = config_with(
            Some("local-default"),
            vec![provider("local", Some("local-default"))],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "local-default");
        assert_eq!(r.provider.as_deref(), Some("local"));
    }

    #[test]
    fn resolve_model_first_provider_default() {
        let config = config_with(None, vec![provider("openrouter", Some("claude-sonnet-4"))]);
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "claude-sonnet-4");
        assert_eq!(r.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn resolve_model_falls_back_to_builtin_default() {
        let config = config_with(None, vec![]);
        let r = resolve_model(&config, None);
        assert_eq!(r.id, rho_ai::catalog::DEFAULT_MODEL_ID);
        assert!(r.is_builtin_default);
        assert!(r.catalog_model.is_some());
    }

    #[test]
    fn resolve_model_catalog_enriches_known_model() {
        let config = config_with(Some("anthropic/claude-sonnet-4"), vec![]);
        let r = resolve_model(&config, None);
        let cat = r.catalog_model.expect("known model is enriched");
        assert_eq!(cat.id, "anthropic/claude-sonnet-4");
        assert!(cat.context_window > 0);
    }

    #[test]
    fn resolve_reasoning_effort_passes_through_without_catalog() {
        // No catalog model → the configured effort is used verbatim.
        assert_eq!(
            resolve_reasoning_effort(None, Some("high")),
            Some("high".into())
        );
    }

    #[test]
    fn resolve_reasoning_effort_kept_for_thinking_model() {
        let thinking = Catalog::resolve(None, "anthropic/claude-sonnet-4");
        assert!(thinking.is_some_and(|m| m.thinking.supported));
        assert_eq!(
            resolve_reasoning_effort(thinking, Some("high")),
            Some("high".into())
        );
    }

    #[test]
    fn switch_model_error_messages_name_the_offender() {
        assert!(
            SwitchModelError::ModelNotFound {
                model: "z.ai/x".into(),
                suggestions: String::new(),
            }
            .message()
            .contains("z.ai/x")
        );
        // Suggestions append a "Did you mean" block to the message.
        assert!(
            SwitchModelError::ModelNotFound {
                model: "qwen3-8".into(),
                suggestions: "  qwen3-8b (provider: local)".into(),
            }
            .message()
            .contains("Did you mean:\n  qwen3-8b (provider: local)")
        );
        assert!(
            SwitchModelError::UnknownProvider {
                provider: "acme".into(),
                model: "acme-7b".into()
            }
            .message()
            .contains("acme")
        );
    }

    fn config_with_provider(
        provider_name: Option<&str>,
        model: Option<&str>,
        providers: Vec<ProviderConfig>,
    ) -> RhoConfig {
        RhoConfig {
            agent: AgentLoopConfig {
                model: model.map(String::from),
                provider: provider_name.map(String::from),
                ..Default::default()
            },
            provider: ProviderSettings { providers },
            ..Default::default()
        }
    }

    #[test]
    fn resolve_model_default_model_when_no_agent_model() {
        let config = config_with(None, vec![provider("local", Some("qwen2.5-coder:7b"))]);
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "qwen2.5-coder:7b");
        assert_eq!(r.provider.as_deref(), Some("local"));
    }

    #[test]
    fn resolve_model_bare_default_resolves_catalog_context_window() {
        let config = config_with(
            None,
            vec![provider("openrouter", Some("deepseek-v4-flash"))],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "deepseek-v4-flash");
        let catalog_model = r
            .catalog_model
            .expect("bare default_model should resolve via basename");
        assert_eq!(catalog_model.id, "deepseek/deepseek-v4-flash");
        assert_eq!(catalog_model.context_window, 1_048_576);
    }

    #[test]
    fn resolve_model_providers_without_default_returns_default() {
        let config = config_with(None, vec![provider("openrouter", None)]);
        let r = resolve_model(&config, None);
        assert_eq!(r.id, rho_ai::catalog::DEFAULT_MODEL_ID);
    }

    #[test]
    fn resolve_model_agent_model_overrides_agent_provider() {
        let config = config_with_provider(
            Some("openai"),
            Some("claude-sonnet-4"),
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "claude-sonnet-4");
        assert_eq!(r.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn resolve_model_agent_provider_selects_providers_default() {
        let config = config_with_provider(
            Some("openrouter"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "claude-sonnet-4");
        assert_eq!(r.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn resolve_model_agent_provider_overrides_first_provider() {
        let config = config_with_provider(
            Some("openai"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "gpt-4o");
        assert_eq!(r.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn resolve_model_agent_provider_unknown_falls_through() {
        let config = config_with_provider(
            Some("unknown"),
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "claude-sonnet-4");
        assert_eq!(r.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn resolve_model_default_with_multiple_providers() {
        let config = config_with(
            None,
            vec![
                provider("openrouter", Some("claude-sonnet-4")),
                provider("openai", Some("gpt-4o")),
            ],
        );
        let r = resolve_model(&config, None);
        assert_eq!(r.id, "claude-sonnet-4");
        assert_eq!(r.provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn resolve_model_catalog_returns_none_for_unknown() {
        let config = config_with(Some("my-custom/local-model"), vec![]);
        let r = resolve_model(&config, None);
        assert!(r.catalog_model.is_none());
    }

    #[test]
    fn resolve_model_default_has_catalog_entry() {
        let config = config_with(None, vec![]);
        let r = resolve_model(&config, None);
        assert!(r.catalog_model.is_some());
        assert_eq!(r.id, rho_ai::catalog::DEFAULT_MODEL_ID);
    }
}
