//! Configuration loading and merging.
//!
//! rho reads configuration from two TOML files, merged with project-level
//! overrides taking precedence over user-level defaults:
//!
//! | Source | Path | Purpose |
//! |---|---|---|
//! | User-level | `~/.rho/config.toml` | Global defaults: default model, API endpoint, provider, provider settings |
//! | Project-level | `.rho/config.toml` (relative to sandbox root) | Per-project: model, approval policies, command denylist, sandbox, context files |
//!
//! # API key handling
//!
//! API keys are **never** stored in plaintext config files. Instead, config
//! references environment variable names:
//!
//! ```toml
//! [provider]
//! api_key_env = "OPENAI_API_KEY"
//! ```
//!
//! The provider reads the key from the env var at runtime. If the env var is
//! not set, the provider reports an error at connection time.
//!
//! # Merging strategy
//!
//! Project-level config overrides user-level config on a per-field basis.
//! For struct fields: if the project sets a field, it wins; if not, the
//! user-level value applies; if neither sets it, the hardcoded default applies.
//!
//! For `Vec` fields (denylist commands, context scan list):
//! the project-level list **replaces** the user-level list, it does not
//! append. This avoids surprising composition effects and keeps overrides
//! predictable.
//!
//! For providers (`[[providers]]`):
//! project-level providers **merge** with user-level providers by name.
//! A project provider with the same name overrides the user's; a new name
//! is added; user providers with no project match are preserved. This lets
//! users configure providers once globally and override selectively per
//! project.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Top-level config ──────────────────────────────────────────────────────────

/// The merged, application-wide configuration.
///
/// Constructed by [`ConfigLoader`] from user-level and project-level TOML
/// files. All fields have sensible defaults so a missing config file is not
/// an error.
#[derive(Clone, Debug, Default)]
pub struct RhoConfig {
    /// Agent loop settings.
    pub agent: AgentLoopConfig,
    /// Model provider settings (one or more providers).
    pub provider: ProviderSettings,
    /// User-defined model pricing entries (`[[models]]`).
    ///
    /// Lets users supply pricing for models the built-in catalog can't
    /// resolve (Groq `-instant` ids, self-hosted, fine-tunes) so cost
    /// accrues instead of surfacing "cost n/a".
    pub models: Vec<UserModelConfig>,
    /// Per-tool approval policies.
    pub approval: ApprovalConfig,
    /// Shell command safety settings.
    pub shell: ShellConfig,
    /// Project context file settings.
    pub context: ContextConfig,
    /// Secret redaction settings.
    pub redaction: RedactionConfig,
    /// System prompt extensions.
    pub system_prompt: SystemPromptConfig,
    /// Extension system settings.
    pub extensions: ExtensionConfig,
    /// Persistent memory (knowledge base) settings.
    pub memory: MemoryConfig,
}

// ── AgentLoopConfig ───────────────────────────────────────────────────────────

/// Agent loop tuning parameters.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentLoopConfig {
    /// Model identifier (e.g. `"qwen3-8b"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Active provider name (e.g. `"openrouter"`, `"local"`).
    ///
    /// When set, this provider is selected at startup. If `model` is
    /// also set, the model is routed to this provider. If `model` is
    /// not set, the provider's `default_model` is used.
    #[serde(default)]
    pub provider: Option<String>,
    /// Maximum model-tool-model round trips before the loop fails.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Maximum *retry attempts* on transient errors before giving up. This is
    /// the number of retries, not the total number of attempts
    /// (initial + retries = 1 + `retry_budget`).
    #[serde(default = "default_retry_budget")]
    pub retry_budget: u32,
    /// Base backoff in milliseconds. Each retry waits
    /// `initial_backoff_ms * 2^retry_number`, capped at 64× the base. So the
    /// first retry waits 2× the base, the second 4×, the third 8×, etc.
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    /// Context window token budget.
    ///
    /// Controls how many tokens the [`SlidingWindowContextManager`] retains
    /// before evicting older turns. Defaults to 32,768.
    ///
    /// The previous default of 8,192 was insufficient: after the system
    /// prompt (~4,700 tokens for `base.md` + `AGENTS.md`), only ~3,500
    /// tokens remained for conversation — barely 1–2 tool-call rounds.
    ///
    /// [`SlidingWindowContextManager`]: crate::context::SlidingWindowContextManager
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    /// Tokens reserved for the model's completion (output) in each request.
    /// Sent as `max_tokens` in every LLM API call. Defaults to 8,192.
    ///
    /// Reasoning models (Qwen3, DeepSeek-R1, etc.) can spend thousands of
    /// tokens on chain-of-thought before producing their response. If the
    /// reserve is too small, the model is cut off mid-reasoning and the
    /// user sees "ran out of tokens" despite having plenty of context.
    /// Increase this for reasoning models (e.g. 16,384 or 32,768).
    #[serde(default = "default_completion_reserve")]
    pub completion_reserve: u32,
    /// Number of consecutive identical (`tool_name`, `arguments`, `output`)
    /// repetitions before the agent injects a stuck-loop nudge.
    /// Set to 0 to disable. Defaults to 3.
    #[serde(default = "default_stuck_loop_threshold")]
    pub stuck_loop_threshold: u32,
    /// Maximum number of consecutive empty model responses before aborting.
    /// Defaults to 5. Set to 0 to allow unlimited empty retries.
    #[serde(default = "default_max_consecutive_empty")]
    pub max_consecutive_empty: u32,
    /// Whether to display chain-of-thought reasoning from reasoning models
    /// (DeepSeek-R1, Qwen3, etc.) in the REPL output.
    ///
    /// - `false` (default): show a one-line summary with the reasoning
    ///   length (e.g. `[reasoning: ~2k tokens]`). The full content is
    ///   available in `tracing` logs at `info` level.
    /// - `true`: show the full reasoning wrapped in `<thinking>` tags
    ///   before the final output.
    #[serde(default = "default_show_reasoning")]
    pub show_reasoning: bool,
    /// Context utilization percentage (0–100) at which the agent loop
    /// automatically compacts older entries to free context space.
    /// Compaction runs proactively *before* eviction is needed.
    /// Set to 0 to disable (default).
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_compact_threshold: u8,
    /// Compaction strategy: `"mechanical"` (default, no LLM calls) or
    /// `"llm"` (uses the configured model to generate narrative summaries).
    ///
    /// When set to `"llm"`, compaction calls the model to produce a concise
    /// narrative of the compacted session segment, stored in the `notes` field
    /// of `CompactionSummary`. If the LLM call fails, falls back to
    /// mechanical compaction (no error propagation).
    #[serde(default = "default_compaction_mode")]
    pub compaction_mode: String,
    /// Reasoning effort for thinking-capable models.
    ///
    /// Passed as `reasoning_effort` in every Chat Completions request.
    /// Common values: `"low"`, `"medium"`, `"high"`. `None` means
    /// no reasoning parameter is sent (default).
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Maximum seconds to wait for the **first** stream event after sending
    /// a request (a "first-token" timeout). Defaults to 90. Set to 0 to
    /// disable.
    ///
    /// This guards against hung connections where the model server (or an
    /// upstream proxy) accepts the request but never emits a first token — a
    /// failure mode that otherwise blocks the agent loop until the upstream's
    /// own idle timeout closes the connection (often surfacing as a silent
    /// empty response). On timeout the request is retried with backoff.
    ///
    /// Reasoning models (DeepSeek-R1, Qwen3, o1-style) can legitimately
    /// spend a long time before the first token; raise this if you see
    /// spurious `StreamTimeout` retries on such models.
    #[serde(default = "default_first_token_timeout_secs")]
    pub first_token_timeout_secs: u64,
    /// Maximum gap (seconds) allowed between consecutive stream events once
    /// streaming has started (an "idle" timeout). Defaults to 60. Set to 0
    /// to disable.
    ///
    /// During healthy generation, events arrive steadily (sub-second to a
    /// few seconds apart). A long gap indicates a dead or stalled
    /// connection; on timeout the request is retried with backoff.
    #[serde(default = "default_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            model: None,
            provider: None,
            max_iterations: default_max_iterations(),
            retry_budget: default_retry_budget(),
            initial_backoff_ms: default_initial_backoff_ms(),
            token_budget: default_token_budget(),
            completion_reserve: default_completion_reserve(),
            stuck_loop_threshold: default_stuck_loop_threshold(),
            max_consecutive_empty: default_max_consecutive_empty(),
            show_reasoning: default_show_reasoning(),
            auto_compact_threshold: default_auto_compact_threshold(),
            compaction_mode: default_compaction_mode(),
            reasoning_effort: None,
            first_token_timeout_secs: default_first_token_timeout_secs(),
            stream_idle_timeout_secs: default_stream_idle_timeout_secs(),
        }
    }
}

/// Default value for `max_iterations`.
fn default_max_iterations() -> u32 {
    32
}
/// Default value for `retry_budget`.
fn default_retry_budget() -> u32 {
    4
}
/// Default value for `initial_backoff_ms`.
fn default_initial_backoff_ms() -> u64 {
    500
}
/// Default value for `token_budget`.
///
/// 32,768 tokens leaves ~28,000 tokens for conversation after the system
/// prompt (~4,700 tokens), compared to ~3,500 with the old 8K default.
fn default_token_budget() -> u32 {
    32_768
}
/// Default value for `completion_reserve`.
///
/// 8,192 tokens is sufficient for non-reasoning models. Reasoning models
/// may need 16,384–32,768 or more.
fn default_completion_reserve() -> u32 {
    8_192
}
/// Default value for `stuck_loop_threshold`.
fn default_stuck_loop_threshold() -> u32 {
    3
}
/// Default value for `max_consecutive_empty`.
fn default_max_consecutive_empty() -> u32 {
    5
}
/// Default value for `show_reasoning`.
fn default_show_reasoning() -> bool {
    false
}

/// Default value for `auto_compact_threshold`.
fn default_auto_compact_threshold() -> u8 {
    0
}

/// Default value for `compaction_mode`.
fn default_compaction_mode() -> String {
    "mechanical".to_owned()
}

/// Default value for `first_token_timeout_secs`.
///
/// 90 seconds is below the common 120-second idle timeout imposed by many
/// upstream proxies/load balancers, so rho aborts and retries a hung request
/// *before* the connection is silently dropped. Generous enough for most
/// reasoning models' first-token latency.
fn default_first_token_timeout_secs() -> u64 {
    90
}

/// Default value for `stream_idle_timeout_secs`.
///
/// Once streaming has started, events arrive steadily; a 60-second gap
/// indicates a dead connection worth retrying.
fn default_stream_idle_timeout_secs() -> u64 {
    60
}

// ── ProviderConfig ────────────────────────────────────────────────────────────

/// Wire protocol used for OpenAI-shaped model requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiProtocol {
    /// OpenAI-compatible `/v1/chat/completions` requests.
    #[default]
    ChatCompletions,
    /// `OpenAI`'s native `/v1/responses` requests.
    Responses,
}

/// Settings for a single model provider.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Provider name for display and selection (e.g. `"local"`, `"openrouter"`).
    ///
    /// Used by `/model` for disambiguation when multiple providers have the
    /// same model. Defaults to the provider index (`"0"`, `"1"`, …) if not
    /// set, or to the preset's name if a preset is configured.
    #[serde(default)]
    pub name: Option<String>,
    /// Provider preset name (e.g. `"lm-studio"`, `"openrouter"`, `"openai"`).
    ///
    /// Fills in `endpoint` and `name` when not explicitly set. If both
    /// `preset` and `endpoint` are set, `endpoint` wins (the preset is
    /// informational only).
    ///
    /// Built-in presets: `lm-studio`, `ollama`, `openrouter`, `openai`,
    /// `groq`, `zai`. An unknown preset logs a warning and is treated as
    /// if no preset was set.
    #[serde(default)]
    pub preset: Option<String>,
    /// Provider type label — informational only, has no effect on behavior.
    ///
    /// rho uses `endpoint` and `api_key_env` to determine how to connect;
    /// it does not branch on this field. Set to any string for your own
    /// bookkeeping (e.g. `"openai"`, `"groq"`, `"production"`), or omit
    /// it entirely.
    #[serde(default)]
    pub r#type: Option<String>,
    /// API endpoint URL.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Request protocol.
    ///
    /// Missing values use Chat Completions unless a provider preset supplies
    /// a different default. The built-in `openai` preset uses Responses.
    #[serde(default)]
    pub api: Option<ApiProtocol>,
    /// Environment variable name holding the API key.
    ///
    /// The key itself is never stored in config; this field names the env var
    /// to read at runtime. Presets do **not** auto-inject this field — the user
    /// must explicitly set it. A hint is logged at startup if a preset suggests
    /// a key and none is configured.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Default model for this provider.
    ///
    /// Used as the model identifier when no `agent.model` or `--model`
    /// is set and this is the first (default) provider. Also used by
    /// model resolution to route a model string to the correct provider.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Optional models endpoint URL, used for model discovery.
    ///
    /// When set, `list_models()` uses this URL directly instead of deriving
    /// one from the chat-completions endpoint. Some providers (e.g. Z.ai)
    /// serve the models list at a different path prefix than chat completions.
    /// When unset, the models URL is derived by replacing `/chat/completions`
    /// with `/models` in the chat endpoint.
    #[serde(default)]
    pub models_endpoint: Option<String>,
}

// ── ProviderSettings ──────────────────────────────────────────────────────────

/// Multi-provider configuration.
///
/// Wraps an ordered list of [`ProviderConfig`] entries. The first entry is
/// the default provider used by `--endpoint` and `--api-key-env` overrides.
///
/// # Empty settings
///
/// When no providers are configured (neither `[[providers]]` nor legacy
/// `[provider]`), the `ProviderRegistry` falls back to a default localhost
/// provider at `http://localhost:1234/v1/chat/completions`.
#[derive(Clone, Debug, Default)]
pub struct ProviderSettings {
    /// Ordered list of provider configurations. The first entry is the
    /// default provider.
    pub providers: Vec<ProviderConfig>,
}

// ── UserModelConfig ───────────────────────────────────────────────────────────

/// A user-defined model pricing entry (`[[models]]`).
///
/// Supplies per-million-token pricing for a model the built-in catalog
/// can't resolve — e.g. a Groq `-instant` id, a self-hosted fine-tune, or
/// any model whose id doesn't match the OpenRouter-derived catalog. Cost
/// resolution consults these (by exact id) before the built-in catalog, so
/// a session whose model matches an entry accrues cost instead of showing
/// "cost n/a".
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UserModelConfig {
    /// Exact model id matched against the session's model string.
    pub id: String,
    /// Optional provider slug (informational; reserved for future
    /// disambiguation).
    #[serde(default)]
    pub provider: Option<String>,
    /// USD per million input tokens.
    pub input_price: f64,
    /// USD per million output tokens.
    pub output_price: f64,
    /// USD per million cached-read input tokens (prompt caching), if the
    /// provider reports cache reads. Optional.
    #[serde(default)]
    pub cached_price: Option<f64>,
}

impl UserModelConfig {
    /// Convert to a catalog [`rho_ai::Model`] for cost resolution.
    ///
    /// Only pricing-relevant fields are populated; context window, max
    /// tokens, and modalities default, since cost computation uses `cost`
    /// alone.
    #[must_use]
    pub fn to_catalog_model(&self) -> rho_ai::Model {
        rho_ai::Model {
            id: self.id.clone(),
            name: self.id.clone(),
            provider: self.provider.clone().unwrap_or_default(),
            context_window: 0,
            max_tokens: 0,
            input: rho_ai::ModelInput::default(),
            cost: rho_ai::ModelCost {
                input: self.input_price,
                output: self.output_price,
                cache_read: self.cached_price.unwrap_or(0.0),
                cache_write: 0.0,
            },
            thinking: rho_ai::ModelThinking::default(),
        }
    }
}

impl ProviderSettings {
    /// The default (first) provider, or `None` if empty.
    #[must_use]
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.providers.first()
    }

    /// The default provider's endpoint, if configured.
    #[must_use]
    pub fn default_endpoint(&self) -> Option<&str> {
        self.providers.first().and_then(|p| p.endpoint.as_deref())
    }

    /// The default provider's API key env var, if configured.
    #[must_use]
    pub fn default_api_key_env(&self) -> Option<&str> {
        self.providers
            .first()
            .and_then(|p| p.api_key_env.as_deref())
    }

    /// Whether any providers are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The default (first) provider's default model, if configured.
    #[must_use]
    pub fn default_model(&self) -> Option<&str> {
        self.providers
            .first()
            .and_then(|p| p.default_model.as_deref())
    }
}

// ── ApprovalConfig ────────────────────────────────────────────────────────────

/// Per-tool approval policies.
///
/// Each tool can be set to one of:
/// - `"auto"` — always execute without asking
/// - `"ask"` — require human confirmation before execution
/// - `"deny"` — refuse to execute the tool at all
///
/// If a tool is not listed, the default policy (based on `ToolRisk`) applies.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ApprovalConfig {
    /// Per-tool policy overrides. Key is the tool name, value is `"auto"`,
    // `"ask"`, or `"deny"`.
    #[serde(default)]
    pub per_tool: std::collections::HashMap<String, ApprovalAction>,
}

/// An approval action for a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    /// Always execute without asking.
    Auto,
    /// Require human confirmation.
    Ask,
    /// Refuse to execute.
    Deny,
}

// ── ShellConfig ───────────────────────────────────────────────────────────────

/// Shell command safety settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ShellConfig {
    /// Additional command names to deny (appended to the built-in denylist).
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// Additional denied flag combinations. Each inner vec is a set of flags;
    /// all flags in the combo must be present to deny.
    #[serde(default)]
    pub denied_flag_combos: Vec<Vec<String>>,
}

/// Default value for boolean flags that default to `true`.
fn default_true() -> bool {
    true
}

// ── ContextConfig ─────────────────────────────────────────────────────────────

/// Project context file scanning settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ContextConfig {
    /// File names to scan for project context, in priority order.
    ///
    /// If set, overrides the default scan list (`AGENTS.md`, `.agents.md`,
    /// `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`).
    #[serde(default)]
    pub scan_list: Option<Vec<String>>,
}

// ── RedactionConfig ───────────────────────────────────────────────────────────

/// Secret redaction settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedactionConfig {
    /// Whether secret redaction is enabled. Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Custom regex patterns to redact, in addition to the built-in patterns.
    ///
    /// Each pattern is a regular expression string. Any text matching a
    /// custom pattern is replaced with `[REDACTED]`. Invalid regex patterns
    /// are silently ignored (a warning is printed to stderr at startup).
    ///
    /// The project-level list **replaces** the user-level list (no appending).
    #[serde(default)]
    pub custom_patterns: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            custom_patterns: Vec::new(),
        }
    }
}

// ── SystemPromptConfig ────────────────────────────────────────────────────────

/// System prompt extension settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SystemPromptConfig {
    /// Additional prompt fragments appended to the base system prompt.
    ///
    /// Each fragment is added after the base prompt and any project context
    /// files, in the order listed.
    #[serde(default)]
    pub extensions: Vec<String>,
}

// ── ExtensionConfig ───────────────────────────────────────────────────────────

/// Extension system configuration.
///
/// Controls which extensions are loaded and what permissions they have.
///
/// # TOML format
///
/// ```toml
/// [extensions]
/// enabled = ["crates-search", "rust-docs"]
/// disabled = ["experimental-thing"]
///
/// [extensions.defaults]
/// network = true
/// max_memory_mb = 64
/// max_execution_time_s = 30
///
/// [extensions.per_extension."rust-docs"]
/// max_memory_mb = 128
/// ```
///
/// # Semantics
///
/// - If `enabled` is set, **only** those extensions are loaded.
/// - If `enabled` is empty/unset, all discovered extensions are candidates.
/// - If `disabled` is set, those extensions are excluded (even if in `enabled`).
/// - `defaults` provides fallback permissions for any extension that doesn't
///   have a `per_extension` override.
/// - `per_extension` overrides are keyed by extension name.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExtensionConfig {
    /// Allowlist: if non-empty, only these extension names are loaded.
    ///
    /// When unset/empty, all discovered extensions are candidates (subject
    /// to `disabled`).
    #[serde(default)]
    pub enabled: Vec<String>,
    /// Denylist: these extension names are never loaded, even if in `enabled`.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Default permissions applied to all extensions.
    #[serde(default)]
    pub defaults: ExtensionPermissions,
    /// Per-extension permission overrides, keyed by extension name.
    #[serde(default)]
    pub per_extension: std::collections::HashMap<String, ExtensionPermissions>,
}

impl ExtensionConfig {
    /// Returns `true` if the given extension name should be loaded.
    ///
    /// An extension is loaded when:
    /// - `enabled` is empty OR the name is in `enabled`
    /// - AND the name is NOT in `disabled`
    ///
    /// `disabled` takes priority over `enabled`.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        let in_enabled = self.enabled.is_empty() || self.enabled.iter().any(|n| n == name);
        let in_disabled = self.disabled.iter().any(|n| n == name);
        in_enabled && !in_disabled
    }

    /// Resolve the effective permissions for a named extension.
    ///
    /// Per-extension overrides are merged on top of `defaults`:
    /// any field set in `per_extension[name]` wins; unset fields fall through
    /// to `defaults`.
    #[must_use]
    pub fn permissions_for(&self, name: &str) -> ExtensionPermissions {
        let mut perms = self.defaults.clone();
        if let Some(override_perms) = self.per_extension.get(name) {
            if override_perms.network.is_some() {
                perms.network = override_perms.network;
            }
            if override_perms.commands.is_some() {
                perms.commands = override_perms.commands;
            }
            if override_perms.max_memory_mb.is_some() {
                perms.max_memory_mb = override_perms.max_memory_mb;
            }
            if override_perms.max_execution_time_s.is_some() {
                perms.max_execution_time_s = override_perms.max_execution_time_s;
            }
            if override_perms.allow_paths.is_some() {
                perms.allow_paths.clone_from(&override_perms.allow_paths);
            }
        }
        perms
    }
}

/// Permissions for a single extension.
///
/// All fields are optional — when `None`, the value falls through to
/// `ExtensionConfig::defaults` or the hardcoded default.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExtensionPermissions {
    /// Whether the extension may access the network (e.g. `host.fetchUrl`).
    ///
    /// `None` means "use the default".
    #[serde(default)]
    pub network: Option<bool>,
    /// Whether the extension may run shell commands (`host.runCommand`).
    ///
    /// `None` means "use the default".
    #[serde(default)]
    pub commands: Option<bool>,
    /// Maximum memory (in MiB) the extension's V8 isolate may use.
    ///
    /// `None` means "use the default" (64 MiB).
    #[serde(default)]
    pub max_memory_mb: Option<u32>,
    /// Maximum execution time (in seconds) for a single tool call.
    ///
    /// `None` means "use the default" (30 s).
    #[serde(default)]
    pub max_execution_time_s: Option<u32>,
    /// Extra directory paths the extension may read/write via `rho.readFile`
    /// and `rho.writeFile` (in addition to its own root directory).
    ///
    /// Paths are resolved relative to the project root. Non-existent paths
    /// are silently ignored.
    ///
    /// `None` means "use the default" (no extra paths).
    #[serde(default)]
    pub allow_paths: Option<Vec<String>>,
}

// ── MemoryConfig ────────────────────────────────────────────────────────

/// Persistent memory (knowledge base) settings.
///
/// Controls whether the project-local knowledge base is enabled.
/// When enabled, the agent gains tools to store and recall knowledge
/// across sessions in a `SQLite` database at `.rho/memory.db`.
///
/// # TOML format
///
/// ```toml
/// [memory]
/// enabled = true
/// ```
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MemoryConfig {
    /// Whether the memory system is enabled. Defaults to `false`.
    #[serde(default)]
    pub enabled: bool,
}

// ── Wire format (TOML-deserializable) ───────────────────���─────────────────────
// ── Wire format (TOML-deserializable) ─────────────────────────────────────────

/// The TOML wire format for a config file.
///
/// All fields are optional so partial configs work naturally. Missing fields
/// fall through to defaults or to the other config tier.
///
/// Supports both the legacy `[provider]` single-table format and the new
/// `[[providers]]` array-of-tables format. When both are present, the
/// array format takes precedence.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireConfig {
    /// Agent loop settings.
    #[serde(default)]
    agent: Option<WireAgentLoopConfig>,
    /// Legacy single-provider config (`[provider]`).
    #[serde(default)]
    provider: Option<ProviderConfig>,
    /// New multi-provider config (`[[providers]]`).
    #[serde(default)]
    providers: Option<Vec<ProviderConfig>>,
    /// User-defined model pricing (`[[models]]`).
    #[serde(default)]
    models: Option<Vec<UserModelConfig>>,
    /// Approval settings.
    #[serde(default)]
    approval: Option<ApprovalConfig>,
    /// Shell settings.
    #[serde(default)]
    shell: Option<ShellConfig>,
    /// Context file settings.
    #[serde(default)]
    context: Option<ContextConfig>,
    /// Redaction settings.
    #[serde(default)]
    redaction: Option<WireRedactionConfig>,
    /// System prompt settings.
    #[serde(default)]
    system_prompt: Option<SystemPromptConfig>,
    /// Extension system settings.
    #[serde(default)]
    extensions: Option<WireExtensionConfig>,
    /// Memory system settings.
    #[serde(default)]
    memory: Option<MemoryConfig>,
}

/// Wire format for `[redaction]` section.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireRedactionConfig {
    /// Whether redaction is enabled.
    #[serde(default)]
    enabled: Option<bool>,
    /// Custom regex patterns.
    #[serde(default)]
    custom_patterns: Option<Vec<String>>,
}

/// Wire format for `[extensions]` section.
///
/// Mirrors [`ExtensionConfig`] but with all fields optional for clean
/// merge semantics.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireExtensionConfig {
    /// Allowlist.
    #[serde(default)]
    enabled: Option<Vec<String>>,
    /// Denylist.
    #[serde(default)]
    disabled: Option<Vec<String>>,
    /// Default permissions.
    #[serde(default)]
    defaults: Option<ExtensionPermissions>,
    /// Per-extension overrides.
    #[serde(default)]
    per_extension: Option<std::collections::HashMap<String, ExtensionPermissions>>,
}

/// Wire format for `[agent]` section.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireAgentLoopConfig {
    /// Model identifier.
    #[serde(default)]
    model: Option<String>,
    /// Active provider name.
    ///
    /// When set, this provider is used at startup and its `default_model`
    /// is used if no `model` is set. The model string is sent to this
    /// provider specifically.
    #[serde(default)]
    provider: Option<String>,
    /// Max iterations before loop fails.
    #[serde(default)]
    max_iterations: Option<u32>,
    /// Retry budget for transient errors.
    #[serde(default)]
    retry_budget: Option<u32>,
    /// Base backoff in milliseconds.
    #[serde(default)]
    initial_backoff_ms: Option<u64>,
    /// Context window token budget.
    #[serde(default)]
    token_budget: Option<u32>,
    /// Completion (output) reserve.
    #[serde(default)]
    completion_reserve: Option<u32>,
    /// Stuck-loop detection threshold.
    #[serde(default)]
    stuck_loop_threshold: Option<u32>,
    /// Maximum consecutive empty responses.
    #[serde(default)]
    max_consecutive_empty: Option<u32>,
    /// Whether to display full reasoning content.
    #[serde(default)]
    show_reasoning: Option<bool>,
    /// Auto-compact threshold percentage.
    #[serde(default)]
    auto_compact_threshold: Option<u8>,
    /// Compaction mode ("mechanical" or "llm").
    #[serde(default)]
    compaction_mode: Option<String>,
    /// Reasoning effort for thinking-capable models.
    #[serde(default)]
    reasoning_effort: Option<String>,
    /// First-token stream timeout in seconds.
    #[serde(default)]
    first_token_timeout_secs: Option<u64>,
    /// Inter-chunk idle stream timeout in seconds.
    #[serde(default)]
    stream_idle_timeout_secs: Option<u64>,
}

// ── Provider presets ───────────────────────────────────────────────────────

/// A named preset for a known model provider.
///
/// Provides default endpoint and display name. The `api_key_env` is
/// informational — used to log a hint at startup, **not** auto-injected.
struct ProviderPreset {
    /// Display name (used when config omits `name`).
    name: &'static str,
    /// Request protocol.
    api: ApiProtocol,
    /// Default endpoint URL.
    endpoint: &'static str,
    /// Suggested env var for the API key (logged as a hint, not applied).
    api_key_env: &'static str,
    /// Optional models endpoint URL, for providers whose models endpoint
    /// is at a different path prefix than the chat-completions endpoint.
    /// When empty, the models URL is derived from the chat endpoint.
    models_endpoint: &'static str,
}

/// Built-in provider presets.
///
/// Lookup by preset name (lowercase). Unknown presets produce a warning
/// at startup and are treated as if no preset was set.
fn presets() -> &'static std::collections::HashMap<&'static str, ProviderPreset> {
    static PRESETS: std::sync::OnceLock<std::collections::HashMap<&'static str, ProviderPreset>> =
        std::sync::OnceLock::new();
    PRESETS.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "lm-studio",
            ProviderPreset {
                name: "lm-studio",
                api: ApiProtocol::ChatCompletions,
                endpoint: "http://localhost:1234/v1/chat/completions",
                api_key_env: "",
                models_endpoint: "",
            },
        );
        m.insert(
            "ollama",
            ProviderPreset {
                name: "ollama",
                api: ApiProtocol::ChatCompletions,
                endpoint: "http://localhost:11434/v1/chat/completions",
                api_key_env: "",
                models_endpoint: "",
            },
        );
        m.insert(
            "openrouter",
            ProviderPreset {
                name: "openrouter",
                api: ApiProtocol::ChatCompletions,
                endpoint: "https://openrouter.ai/api/v1/chat/completions",
                api_key_env: "OPENROUTER_API_KEY",
                models_endpoint: "",
            },
        );
        m.insert(
            "openai",
            ProviderPreset {
                name: "openai",
                api: ApiProtocol::Responses,
                endpoint: "https://api.openai.com/v1/responses",
                api_key_env: "OPENAI_API_KEY",
                models_endpoint: "",
            },
        );
        m.insert(
            "groq",
            ProviderPreset {
                name: "groq",
                api: ApiProtocol::ChatCompletions,
                endpoint: "https://api.groq.com/openai/v1/chat/completions",
                api_key_env: "GROQ_API_KEY",
                models_endpoint: "",
            },
        );
        m.insert(
            "zai",
            ProviderPreset {
                name: "zai",
                api: ApiProtocol::ChatCompletions,
                endpoint: "https://api.z.ai/api/paas/v4/chat/completions",
                api_key_env: "ZAI_API_KEY",
                models_endpoint: "https://api.z.ai/api/v1/models",
            },
        );
        m.insert(
            "zai-coding",
            ProviderPreset {
                // Same `name` as the platform preset so `z.ai/...` model
                // routing stays consistent; the two are mutually exclusive
                // in practice (pay-as-you-go platform vs Coding Plan).
                name: "zai",
                api: ApiProtocol::ChatCompletions,
                endpoint: "https://api.z.ai/api/coding/paas/v4/chat/completions",
                api_key_env: "ZAI_API_KEY",
                // Empty so the models URL is derived from the coding chat
                // endpoint → `/api/coding/paas/v4/models`.
                models_endpoint: "",
            },
        );
        m
    })
}

/// Resolve preset defaults into provider configurations.
///
/// For each provider that has a `preset` set:
/// - If `endpoint` is not set, use the preset's endpoint.
/// - If `name` is not set, use the preset's name.
/// - If `api_key_env` is not set and the preset suggests one, log a hint
///   (do **not** auto-inject — the user must explicitly set it).
/// - If the preset is unknown, log a warning and skip.
/// - If both `preset` and `endpoint`/`name` are set, the explicit values
///   win (preset only fills gaps).
/// - If `models_endpoint` is unset and the preset provides one, it's used —
///   *unless* `endpoint` was explicitly set to a different URL than the
///   preset's, in which case the preset's `models_endpoint` is skipped (it
///   would target the wrong API, e.g. z.ai's Coding Plan vs platform API)
///   and the models URL is derived from the chat endpoint instead.
fn resolve_presets(providers: &mut [ProviderConfig]) {
    for p in providers {
        let Some(preset_name) = &p.preset else {
            continue;
        };
        let Some(preset) = presets().get(preset_name.as_str()) else {
            tracing::warn!(preset = %preset_name, "unknown provider preset");
            continue;
        };
        // A user-provided endpoint that differs from the preset's means the
        // provider has been pointed elsewhere (e.g. z.ai's Coding Plan uses
        // `/api/coding/...` vs the platform `/api/paas/v4/...`). The preset's
        // `models_endpoint` would then target the wrong API, so skip it and
        // let the models URL be derived from the actual chat endpoint.
        let endpoint_overridden =
            p.endpoint.is_some() && p.endpoint.as_deref() != Some(preset.endpoint);
        if p.endpoint.is_none() {
            p.endpoint = Some(preset.endpoint.to_owned());
        }
        if p.name.is_none() {
            p.name = Some(preset.name.to_owned());
        }
        if p.api.is_none() {
            p.api = Some(preset.api);
        }
        if p.models_endpoint.is_none() && !preset.models_endpoint.is_empty() && !endpoint_overridden
        {
            p.models_endpoint = Some(preset.models_endpoint.to_owned());
        }
        if p.api_key_env.is_none() && !preset.api_key_env.is_empty() {
            tracing::info!(
                provider = preset.name,
                api_key_env = preset.api_key_env,
                "hint: preset provider may require API key"
            );
        }
    }
}

/// Look up the endpoint URL for a built-in provider preset.
///
/// Returns `None` for an unknown preset name. Used by the binary to
/// synthesize a default provider for the built-in default model (the
/// `openrouter` preset for `anthropic/claude-sonnet-4`) without
/// duplicating the preset's endpoint string.
///
/// See [`presets`] for the list of built-in preset names.
#[must_use]
pub fn preset_endpoint(preset: &str) -> Option<&'static str> {
    presets().get(preset).map(|p| p.endpoint)
}

/// Look up the suggested API-key env var for a built-in provider preset.
///
/// Returns `None` for an unknown preset or for presets that need no API key
/// (local servers like `lm-studio` / `ollama`).
#[must_use]
pub fn preset_api_key_env(preset: &str) -> Option<&'static str> {
    presets()
        .get(preset)
        .and_then(|p| (!p.api_key_env.is_empty()).then_some(p.api_key_env))
}

// ── Provider merge ──────────────────────────────────────────────────────────

/// Merge user-level and project-level provider configurations.
///
/// Project-level providers override user-level providers by **name**.
/// A project provider with the same name as a user provider replaces it
/// entirely (all fields come from the project). A project provider with
/// a new name is appended. User providers with no matching project
/// provider are preserved in their original order.
///
/// This allows a user to configure providers once in `~/.rho/config.toml`
/// and have project configs selectively override or add providers without
/// losing the global configuration.
fn merge_providers(user: &[ProviderConfig], project: &[ProviderConfig]) -> Vec<ProviderConfig> {
    let project_names: std::collections::HashSet<&str> =
        project.iter().filter_map(|p| p.name.as_deref()).collect();

    let mut merged = Vec::with_capacity(user.len() + project.len());

    // 1. User providers that are NOT overridden by a project provider.
    for p in user {
        if !project_names.contains(p.name.as_deref().unwrap_or("")) {
            merged.push(p.clone());
        }
    }

    // 2. All project providers (both overrides and new additions).
    //    These come after user providers to maintain deterministic ordering
    //    and ensure project providers are the "active" versions.
    merged.extend(project.iter().cloned());

    merged
}

// ── ConfigLoader ──────────────────────────────────────────────────────────────

/// Loads and merges rho configuration from user-level and project-level files.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load configuration from both tiers and merge them.
    ///
    /// 1. Load user-level config from `~/.rho/config.toml` (missing is OK).
    /// 2. Load project-level config from `<root>/.rho/config.toml` (missing is OK).
    /// 3. Merge: project-level fields override user-level fields.
    /// 4. Return the merged [`RhoConfig`].
    ///
    /// # Errors
    ///
    /// Returns an error if a config file exists but cannot be parsed as valid
    /// TOML. A missing config file is not an error.
    pub fn load(root: &Path) -> Result<RhoConfig, ConfigLoadError> {
        let user_path = user_config_path();
        let project_path = root.join(".rho").join("config.toml");

        let user = Self::load_file(&user_path)?;
        let project = Self::load_file(&project_path)?;

        Ok(Self::merge(user, project))
    }

    /// Load a single config file, returning `None` if it doesn't exist.
    fn load_file(path: &Path) -> Result<Option<WireConfig>, ConfigLoadError> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path).map_err(|e| ConfigLoadError {
            path: path.to_path_buf(),
            source: Box::new(ConfigLoadErrorKind::Io(e)),
        })?;
        let config: WireConfig = toml::from_str(&text).map_err(|e| ConfigLoadError {
            path: path.to_path_buf(),
            source: Box::new(ConfigLoadErrorKind::Parse(e)),
        })?;
        Ok(Some(config))
    }

    /// Merge user-level and project-level wire configs into a `RhoConfig`.
    ///
    /// Project-level fields override user-level fields. `None` fields fall
    /// through to the other tier, then to hardcoded defaults.
    ///
    /// For providers: project `[[providers]]` > user `[[providers]]` >
    /// project `[provider]` > user `[provider]` > empty. Project-level
    /// `[[providers]]` **merges** with user-level by provider name: a project
    /// provider with the same name overrides the user's, a new name is added,
    /// and user providers with no project match are preserved.
    #[allow(clippy::too_many_lines)]
    fn merge(user: Option<WireConfig>, project: Option<WireConfig>) -> RhoConfig {
        let user = user.unwrap_or_default();
        let project = project.unwrap_or_default();
        let user_agent = user.agent.unwrap_or_default();
        let project_agent = project.agent.unwrap_or_default();

        RhoConfig {
            agent: AgentLoopConfig {
                model: project_agent.model.or(user_agent.model),
                provider: project_agent.provider.or(user_agent.provider),
                max_iterations: project_agent
                    .max_iterations
                    .or(user_agent.max_iterations)
                    .unwrap_or(default_max_iterations()),
                retry_budget: project_agent
                    .retry_budget
                    .or(user_agent.retry_budget)
                    .unwrap_or(default_retry_budget()),
                initial_backoff_ms: project_agent
                    .initial_backoff_ms
                    .or(user_agent.initial_backoff_ms)
                    .unwrap_or(default_initial_backoff_ms()),
                token_budget: project_agent
                    .token_budget
                    .or(user_agent.token_budget)
                    .unwrap_or(default_token_budget()),
                completion_reserve: project_agent
                    .completion_reserve
                    .or(user_agent.completion_reserve)
                    .unwrap_or(default_completion_reserve()),
                stuck_loop_threshold: project_agent
                    .stuck_loop_threshold
                    .or(user_agent.stuck_loop_threshold)
                    .unwrap_or(default_stuck_loop_threshold()),
                max_consecutive_empty: project_agent
                    .max_consecutive_empty
                    .or(user_agent.max_consecutive_empty)
                    .unwrap_or(default_max_consecutive_empty()),
                show_reasoning: project_agent
                    .show_reasoning
                    .or(user_agent.show_reasoning)
                    .unwrap_or(default_show_reasoning()),
                auto_compact_threshold: project_agent
                    .auto_compact_threshold
                    .or(user_agent.auto_compact_threshold)
                    .unwrap_or(default_auto_compact_threshold()),
                compaction_mode: project_agent
                    .compaction_mode
                    .clone()
                    .or(user_agent.compaction_mode.clone())
                    .unwrap_or(default_compaction_mode()),
                reasoning_effort: project_agent
                    .reasoning_effort
                    .clone()
                    .or(user_agent.reasoning_effort.clone()),
                first_token_timeout_secs: project_agent
                    .first_token_timeout_secs
                    .or(user_agent.first_token_timeout_secs)
                    .unwrap_or(default_first_token_timeout_secs()),
                stream_idle_timeout_secs: project_agent
                    .stream_idle_timeout_secs
                    .or(user_agent.stream_idle_timeout_secs)
                    .unwrap_or(default_stream_idle_timeout_secs()),
            },
            provider: {
                // New `[[providers]]` format takes precedence over legacy `[provider]`.
                // Project-level merges with user-level by name: a project provider
                // with the same name overrides the user's, a new name is added,
                // and user providers with no project match are preserved.
                let pp = project.providers.unwrap_or_default();
                let up = user.providers.unwrap_or_default();

                let mut entries = if !pp.is_empty() || !up.is_empty() {
                    merge_providers(&up, &pp)
                } else {
                    // Fall back to legacy single-provider config.
                    let legacy = project.provider.or(user.provider);
                    match legacy {
                        Some(p) => vec![p],
                        None => vec![],
                    }
                };

                // Resolve presets after merge so project-level overrides
                // (e.g. api_key_env) are preserved.
                resolve_presets(&mut entries);

                ProviderSettings { providers: entries }
            },
            models: {
                // Project-level overrides user-level by id; user-only
                // entries are preserved.
                let mut by_id: std::collections::HashMap<String, UserModelConfig> = user
                    .models
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| (m.id.clone(), m))
                    .collect();
                for m in project.models.unwrap_or_default() {
                    by_id.insert(m.id.clone(), m);
                }
                by_id.into_values().collect()
            },
            approval: project.approval.or(user.approval).unwrap_or_default(),
            shell: ShellConfig {
                denied_commands: project
                    .shell
                    .as_ref()
                    .map(|s| s.denied_commands.clone())
                    .or(user.shell.as_ref().map(|s| s.denied_commands.clone()))
                    .unwrap_or_default(),
                denied_flag_combos: project
                    .shell
                    .as_ref()
                    .map(|s| s.denied_flag_combos.clone())
                    .or(user.shell.as_ref().map(|s| s.denied_flag_combos.clone()))
                    .unwrap_or_default(),
            },
            context: project.context.or(user.context).unwrap_or_default(),
            redaction: {
                let ur = user.redaction.unwrap_or_default();
                let pr = project.redaction.unwrap_or_default();
                RedactionConfig {
                    enabled: pr.enabled.or(ur.enabled).unwrap_or(default_true()),
                    custom_patterns: pr
                        .custom_patterns
                        .or(ur.custom_patterns)
                        .unwrap_or_default(),
                }
            },
            system_prompt: project
                .system_prompt
                .or(user.system_prompt)
                .unwrap_or_default(),
            extensions: {
                let ue = user.extensions.unwrap_or_default();
                let pe = project.extensions.unwrap_or_default();
                let user_defaults = ue.defaults.unwrap_or_default();
                let project_defaults = pe.defaults.unwrap_or_default();
                ExtensionConfig {
                    enabled: pe.enabled.or(ue.enabled).unwrap_or_default(),
                    disabled: pe.disabled.or(ue.disabled).unwrap_or_default(),
                    defaults: merge_permissions(user_defaults, project_defaults),
                    per_extension: pe.per_extension.or(ue.per_extension).unwrap_or_default(),
                }
            },
            memory: project.memory.or(user.memory).unwrap_or_default(),
        }
    }
}

// ── ConfigLoadError ───────────────────────────────────────────────────────────

/// An error encountered while loading a configuration file.
#[derive(Debug)]
pub struct ConfigLoadError {
    /// The config file path that caused the error.
    pub path: PathBuf,
    /// What went wrong.
    pub source: Box<ConfigLoadErrorKind>,
}

/// The kind of config load error.
#[derive(Debug)]
pub enum ConfigLoadErrorKind {
    /// An I/O error reading the file.
    Io(std::io::Error),
    /// The file exists but is not valid TOML or doesn't match the expected schema.
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &*self.source {
            ConfigLoadErrorKind::Io(e) => {
                write!(f, "config: cannot read `{}`: {e}", self.path.display())
            }
            ConfigLoadErrorKind::Parse(e) => {
                write!(f, "config: cannot parse `{}`: {e}", self.path.display())
            }
        }
    }
}

impl std::error::Error for ConfigLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &*self.source {
            ConfigLoadErrorKind::Io(e) => Some(e),
            ConfigLoadErrorKind::Parse(e) => Some(e),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the path for the user-level config file (`~/.rho/config.toml`).
pub fn user_config_path() -> PathBuf {
    dirs_home().join(".rho").join("config.toml")
}

/// Merge two sets of extension permissions.
///
/// Project-level fields override user-level fields. `None` fields fall
/// through to the other tier.
fn merge_permissions(
    user: ExtensionPermissions,
    project: ExtensionPermissions,
) -> ExtensionPermissions {
    ExtensionPermissions {
        network: project.network.or(user.network),
        commands: project.commands.or(user.commands),
        max_memory_mb: project.max_memory_mb.or(user.max_memory_mb),
        max_execution_time_s: project.max_execution_time_s.or(user.max_execution_time_s),
        allow_paths: project.allow_paths.or(user.allow_paths),
    }
}

/// Best-effort home directory; falls back to current directory if unavailable.
///
/// Shared with `context_files.rs` — the same logic for finding `~/.rho/`.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("."), PathBuf::from)
}

// ── Resolving helpers ─────────────────────────────────────────────────────────

impl RhoConfig {
    /// Resolve the API key from the configured environment variable.
    ///
    /// Returns `None` if the default provider's `api_key_env` is not set or
    /// the env var doesn't exist. Returns `Some(key)` if the env var is set.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.provider
            .default_api_key_env()
            .and_then(|var| std::env::var(var).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ── ConfigLoader ───────────────────────────────────────────────────────

    #[test]
    fn load_with_no_files_returns_defaults() {
        let dir = TempDir::new().unwrap();
        // Isolate from the user's real ~/.rho/config.toml.
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let config = ConfigLoader::load(dir.path()).unwrap();

                assert!(config.agent.model.is_none());
                assert_eq!(config.agent.max_iterations, 32);
                assert_eq!(config.agent.retry_budget, 4);
                assert_eq!(config.agent.initial_backoff_ms, 500);
                assert_eq!(config.agent.token_budget, 32_768);
                assert!(config.provider.is_empty());
                assert!(config.approval.per_tool.is_empty());
                assert!(config.shell.denied_commands.is_empty());
                assert!(config.context.scan_list.is_none());
                assert!(config.redaction.enabled);
                assert!(config.redaction.custom_patterns.is_empty());
                assert!(config.system_prompt.extensions.is_empty());
            },
        );
    }

    #[test]
    fn load_project_config_overrides_defaults() {
        let dir = TempDir::new().unwrap();
        // Isolate from the user's real ~/.rho/config.toml.
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                let toml_text = r#"
[agent]
model = "gpt-4o"
max_iterations = 10

[provider]
type = "openai"
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"

[approval.per_tool]
run_command = "ask"
write_file = "deny"

[shell]
denied_commands = ["Remove-Item", "Invoke-WebRequest"]

[context]
scan_list = ["AGENTS.md", "CLAUDE.md"]

[redaction]
enabled = false

[system_prompt]
extensions = ["Always use PowerShell 7."]
"#;
                let mut f = std::fs::File::create(rho_dir.join("config.toml")).unwrap();
                f.write_all(toml_text.as_bytes()).unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();

                assert_eq!(config.agent.model.as_deref(), Some("gpt-4o"));
                assert_eq!(config.agent.max_iterations, 10);

                // Legacy [provider] is promoted to single-element ProviderSettings.
                assert_eq!(config.provider.providers.len(), 1);
                let p = config.provider.default_provider().unwrap();
                assert_eq!(p.r#type.as_deref(), Some("openai"));
                assert_eq!(
                    p.endpoint.as_deref(),
                    Some("https://api.openai.com/v1/chat/completions")
                );
                assert_eq!(p.api_key_env.as_deref(), Some("OPENAI_API_KEY"));

                assert_eq!(
                    config.approval.per_tool.get("run_command"),
                    Some(&ApprovalAction::Ask)
                );
                assert_eq!(
                    config.approval.per_tool.get("write_file"),
                    Some(&ApprovalAction::Deny)
                );
                assert_eq!(
                    config.shell.denied_commands,
                    vec!["Remove-Item", "Invoke-WebRequest"]
                );
                assert_eq!(
                    config.context.scan_list,
                    Some(vec!["AGENTS.md".to_owned(), "CLAUDE.md".to_owned()])
                );
                assert!(!config.redaction.enabled);
                assert!(config.redaction.custom_patterns.is_empty());
            },
        );
    }

    #[test]
    fn project_overrides_user() {
        let dir = TempDir::new().unwrap();

        // Create a fake user config dir.
        let user_dir = TempDir::new().unwrap();
        let user_rho = user_dir.path().join(".rho");
        std::fs::create_dir_all(&user_rho).unwrap();

        // Write user-level config.
        std::fs::write(
            user_rho.join("config.toml"),
            r#"
[agent]
model = "user-model"
max_iterations = 20

[provider]
type = "local"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();

        // Write project-level config.
        let project_rho = dir.path().join(".rho");
        std::fs::create_dir_all(&project_rho).unwrap();
        std::fs::write(
            project_rho.join("config.toml"),
            r#"
[agent]
model = "project-model"
"#,
        )
        .unwrap();

        // Load user config.
        let user_config: WireConfig =
            toml::from_str(&std::fs::read_to_string(user_rho.join("config.toml")).unwrap())
                .unwrap();
        let project_config: WireConfig =
            toml::from_str(&std::fs::read_to_string(project_rho.join("config.toml")).unwrap())
                .unwrap();

        let config = ConfigLoader::merge(Some(user_config), Some(project_config));

        // Project model overrides user model.
        assert_eq!(config.agent.model.as_deref(), Some("project-model"));
        // User max_iterations is preserved (project didn't set it).
        assert_eq!(config.agent.max_iterations, 20);
        // User provider settings are preserved (project didn't set provider).
        assert_eq!(config.provider.providers.len(), 1);
        let p = config.provider.default_provider().unwrap();
        assert_eq!(p.r#type.as_deref(), Some("local"));
        assert_eq!(
            p.endpoint.as_deref(),
            Some("http://localhost:1234/v1/chat/completions")
        );
    }

    #[test]
    fn user_config_only_applies_when_no_project_config() {
        let user_config: WireConfig = toml::from_str(
            r#"
[agent]
model = "user-model"
max_iterations = 20
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), None);

        assert_eq!(config.agent.model.as_deref(), Some("user-model"));
        assert_eq!(config.agent.max_iterations, 20);
    }

    #[test]
    fn malformed_toml_returns_error() {
        let dir = TempDir::new().unwrap();
        let rho_dir = dir.path().join(".rho");
        std::fs::create_dir_all(&rho_dir).unwrap();
        std::fs::write(rho_dir.join("config.toml"), "this is not [[valid").unwrap();

        let result = ConfigLoader::load(dir.path());
        assert!(result.is_err(), "malformed TOML should return an error");
        let err = result.unwrap_err();
        assert!(
            matches!(&*err.source, ConfigLoadErrorKind::Parse(_)),
            "expected Parse error, got: {:?}",
            err.source
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let dir = TempDir::new().unwrap();
        let rho_dir = dir.path().join(".rho");
        std::fs::create_dir_all(&rho_dir).unwrap();

        // Unknown keys should not cause a parse error (forward compatibility).
        std::fs::write(
            rho_dir.join("config.toml"),
            r#"
[agent]
model = "test"
future_unknown_field = "surprise"
"#,
        )
        .unwrap();

        let result = ConfigLoader::load(dir.path());
        assert!(result.is_ok(), "unknown scalar fields should be ignored");
    }

    // ── API key resolution ─────────────────────────────────────────────────

    #[test]
    fn resolve_api_key_returns_none_when_not_configured() {
        let config = RhoConfig::default();
        assert!(config.resolve_api_key().is_none());
    }

    #[test]
    fn resolve_api_key_returns_none_when_env_var_not_set() {
        let config = RhoConfig {
            provider: ProviderSettings {
                providers: vec![ProviderConfig {
                    api_key_env: Some("RHO_TEST_NONEXISTENT_KEY_12345".to_owned()),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        assert!(config.resolve_api_key().is_none());
    }

    // ── Approval actions ───────────────────────────────────────────────────

    #[test]
    fn approval_action_serde_round_trip() {
        let actions = vec![
            ApprovalAction::Auto,
            ApprovalAction::Ask,
            ApprovalAction::Deny,
        ];
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let back: ApprovalAction = serde_json::from_str(&json).unwrap();
            assert_eq!(action, back);
        }
    }

    #[test]
    fn approval_action_toml_deserialize() {
        let toml_text = r#"
[per_tool]
read_file = "auto"
write_file = "ask"
run_command = "deny"
"#;
        let config: ApprovalConfig = toml::from_str(toml_text).unwrap();
        assert_eq!(
            config.per_tool.get("read_file"),
            Some(&ApprovalAction::Auto)
        );
        assert_eq!(
            config.per_tool.get("write_file"),
            Some(&ApprovalAction::Ask)
        );
        assert_eq!(
            config.per_tool.get("run_command"),
            Some(&ApprovalAction::Deny)
        );
    }

    // ── Partial config ─────────────────────────────────────────────────────

    #[test]
    fn partial_agent_config_preserves_defaults() {
        let dir = TempDir::new().unwrap();
        // Isolate from the user real ~/.rho/config.toml.
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                // Only set model, everything else should be default.
                std::fs::write(
                    rho_dir.join("config.toml"),
                    r#"
[agent]
model = "test-model"
"#,
                )
                .unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();
                assert_eq!(config.agent.model.as_deref(), Some("test-model"));
                assert_eq!(config.agent.max_iterations, 32);
                assert_eq!(config.agent.retry_budget, 4);
                assert_eq!(config.agent.initial_backoff_ms, 500);
                assert_eq!(config.agent.token_budget, 32_768);
            },
        );
    }

    #[test]
    fn token_budget_from_config() {
        let dir = TempDir::new().unwrap();
        // Isolate from the user real ~/.rho/config.toml.
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                std::fs::write(
                    rho_dir.join("config.toml"),
                    r"
[agent]
token_budget = 65536
",
                )
                .unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();
                assert_eq!(config.agent.token_budget, 65_536);
                // Other agent fields should still be defaults.
                assert_eq!(config.agent.max_iterations, 32);
            },
        );
    }

    #[test]
    fn token_budget_project_overrides_user() {
        let user_config: WireConfig = toml::from_str(
            r"
[agent]
token_budget = 16384
",
        )
        .unwrap();
        let project_config: WireConfig = toml::from_str(
            r"
[agent]
token_budget = 131072
",
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), Some(project_config));
        assert_eq!(config.agent.token_budget, 131_072);
    }

    #[test]
    fn token_budget_user_preserved_when_no_project() {
        let user_config: WireConfig = toml::from_str(
            r"
[agent]
token_budget = 16384
",
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), None);
        assert_eq!(config.agent.token_budget, 16_384);
    }

    #[test]
    fn token_budget_default_is_32k() {
        let config = AgentLoopConfig::default();
        assert_eq!(config.token_budget, 32_768);
    }

    #[test]
    fn completion_reserve_default_is_8k() {
        let config = AgentLoopConfig::default();
        assert_eq!(config.completion_reserve, 8_192);
    }

    #[test]
    fn completion_reserve_from_config() {
        let dir = TempDir::new().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                std::fs::write(
                    rho_dir.join("config.toml"),
                    r"
[agent]
completion_reserve = 16384
",
                )
                .unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();
                assert_eq!(config.agent.completion_reserve, 16_384);
                // Other agent fields should still be defaults.
                assert_eq!(config.agent.token_budget, 32_768);
            },
        );
    }

    #[test]
    fn completion_reserve_project_overrides_user() {
        let user_config: WireConfig = toml::from_str(
            r"
[agent]
completion_reserve = 8192
",
        )
        .unwrap();
        let project_config: WireConfig = toml::from_str(
            r"
[agent]
completion_reserve = 32768
",
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), Some(project_config));
        assert_eq!(config.agent.completion_reserve, 32_768);
    }

    #[test]
    fn completion_reserve_user_preserved_when_no_project() {
        let user_config: WireConfig = toml::from_str(
            r"
[agent]
completion_reserve = 16384
",
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), None);
        assert_eq!(config.agent.completion_reserve, 16_384);
    }

    #[test]
    fn config_with_only_provider_section() {
        let dir = TempDir::new().unwrap();
        // Isolate from the user real ~/.rho/config.toml.
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                std::fs::write(
                    rho_dir.join("config.toml"),
                    r#"
[provider]
type = "local"
endpoint = "http://localhost:8080/v1/chat/completions"
"#,
                )
                .unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();
                // Legacy [provider] promoted to single-element ProviderSettings.
                assert_eq!(config.provider.providers.len(), 1);
                let p = config.provider.default_provider().unwrap();
                assert_eq!(p.r#type.as_deref(), Some("local"));
                assert_eq!(
                    p.endpoint.as_deref(),
                    Some("http://localhost:8080/v1/chat/completions")
                );
                // Everything else is default.
                assert!(config.agent.model.is_none());
            },
        );
    }

    // ── Redaction config ──────────────────────────────────────────────────

    #[test]
    fn redaction_custom_patterns_from_config() {
        let dir = TempDir::new().unwrap();
        let rho_dir = dir.path().join(".rho");
        std::fs::create_dir_all(&rho_dir).unwrap();

        std::fs::write(
            rho_dir.join("config.toml"),
            r#"
[redaction]
enabled = true
custom_patterns = ["my-key-[a-zA-Z0-9]{32}", "token: \\S+"]
"#,
        )
        .unwrap();

        let config = ConfigLoader::load(dir.path()).unwrap();
        assert!(config.redaction.enabled);
        assert_eq!(
            config.redaction.custom_patterns,
            vec!["my-key-[a-zA-Z0-9]{32}", "token: \\S+"]
        );
    }

    #[test]
    fn redaction_project_custom_patterns_replace_user() {
        let user_config: WireConfig = toml::from_str(
            r#"
[redaction]
custom_patterns = ["user-pattern"]
"#,
        )
        .unwrap();
        let project_config: WireConfig = toml::from_str(
            r#"
[redaction]
custom_patterns = ["project-pattern"]
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), Some(project_config));
        assert_eq!(config.redaction.custom_patterns, vec!["project-pattern"]);
    }

    #[test]
    fn redaction_user_patterns_preserved_when_no_project() {
        let user_config: WireConfig = toml::from_str(
            r#"
[redaction]
custom_patterns = ["user-pattern"]
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), None);
        assert_eq!(config.redaction.custom_patterns, vec!["user-pattern"]);
    }

    #[test]
    fn redaction_enabled_from_project_overrides_user() {
        let user_config: WireConfig = toml::from_str(
            r"
[redaction]
enabled = true
",
        )
        .unwrap();
        let project_config: WireConfig = toml::from_str(
            r"
[redaction]
enabled = false
",
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_config), Some(project_config));
        assert!(!config.redaction.enabled);
    }

    #[test]
    fn redaction_defaults_to_enabled_empty_patterns() {
        let config = RedactionConfig::default();
        assert!(config.enabled);
        assert!(config.custom_patterns.is_empty());
    }

    // ── Shell denylist ─────────────────────────────────────────────────────

    #[test]
    fn shell_denied_commands_from_config() {
        let dir = TempDir::new().unwrap();
        let rho_dir = dir.path().join(".rho");
        std::fs::create_dir_all(&rho_dir).unwrap();

        std::fs::write(
            rho_dir.join("config.toml"),
            r#"
[shell]
denied_commands = ["Stop-Process", "Get-Process"]
denied_flag_combos = [["-Quiet", "-Force"]]
"#,
        )
        .unwrap();

        let config = ConfigLoader::load(dir.path()).unwrap();
        assert_eq!(
            config.shell.denied_commands,
            vec!["Stop-Process", "Get-Process"]
        );
        assert_eq!(
            config.shell.denied_flag_combos,
            vec![vec!["-Quiet".to_owned(), "-Force".to_owned()]]
        );
    }

    // ── Multi-provider config ──────────────────────────────────────────────

    #[test]
    fn load_multi_provider_config() {
        let wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "local"
endpoint = "http://localhost:1234/v1/chat/completions"

[[providers]]
name = "openrouter"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .unwrap();

        assert!(wire.provider.is_none());
        let providers = wire.providers.unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name.as_deref(), Some("local"));
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("http://localhost:1234/v1/chat/completions")
        );
        assert_eq!(providers[1].name.as_deref(), Some("openrouter"));
        assert_eq!(
            providers[1].api_key_env.as_deref(),
            Some("OPENROUTER_API_KEY")
        );
    }

    #[test]
    fn legacy_provider_promoted_to_vec() {
        let wire: WireConfig = toml::from_str(
            r#"
[provider]
type = "local"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();

        assert!(wire.providers.is_none());
        let legacy = wire.provider.clone().unwrap();
        assert_eq!(legacy.r#type.as_deref(), Some("local"));

        // Merge promotes to single-element vec.
        let config = ConfigLoader::merge(Some(wire), None);
        assert_eq!(config.provider.providers.len(), 1);
        assert_eq!(
            config.provider.default_endpoint(),
            Some("http://localhost:1234/v1/chat/completions")
        );
    }

    #[test]
    fn providers_array_takes_precedence_over_legacy() {
        let wire: WireConfig = toml::from_str(
            r#"
[provider]
endpoint = "http://legacy:1234/v1/chat/completions"

[[providers]]
name = "modern"
endpoint = "http://modern:1234/v1/chat/completions"
"#,
        )
        .unwrap();

        // Both are present; [[providers]] should win.
        assert!(wire.provider.is_some());
        assert!(wire.providers.is_some());

        let config = ConfigLoader::merge(Some(wire), None);
        assert_eq!(config.provider.providers.len(), 1);
        assert_eq!(
            config.provider.default_provider().unwrap().name.as_deref(),
            Some("modern")
        );
    }

    #[test]
    fn providers_project_merges_with_user() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "user-local"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "project-openrouter"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));
        // Project adds a new provider (different name), user provider preserved.
        assert_eq!(config.provider.providers.len(), 2);
        assert_eq!(
            config.provider.providers[0].name.as_deref(),
            Some("user-local")
        );
        assert_eq!(
            config.provider.providers[1].name.as_deref(),
            Some("project-openrouter")
        );
    }

    #[test]
    fn providers_project_overrides_user_by_name() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "openrouter"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "openrouter"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));
        // Project overrides user provider with the same name.
        assert_eq!(config.provider.providers.len(), 1);
        let p = config.provider.default_provider().unwrap();
        assert_eq!(p.name.as_deref(), Some("openrouter"));
        assert_eq!(p.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn providers_project_only_adds_to_user() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "local"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[agent]
model = "gpt-4o"
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));
        // Project doesn't set any provider; user providers preserved.
        assert_eq!(config.provider.providers.len(), 1);
        assert_eq!(
            config.provider.default_provider().unwrap().name.as_deref(),
            Some("local")
        );
        assert_eq!(config.agent.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn provider_settings_convenience_accessors() {
        let settings = ProviderSettings {
            providers: vec![ProviderConfig {
                name: Some("local".to_owned()),
                endpoint: Some("http://localhost:1234".to_owned()),
                api_key_env: Some("LOCAL_KEY".to_owned()),
                ..Default::default()
            }],
        };

        assert!(!settings.is_empty());
        assert!(settings.default_provider().is_some());
        assert_eq!(settings.default_endpoint(), Some("http://localhost:1234"));
        assert_eq!(settings.default_api_key_env(), Some("LOCAL_KEY"));

        let empty = ProviderSettings::default();
        assert!(empty.is_empty());
        assert!(empty.default_provider().is_none());
        assert!(empty.default_endpoint().is_none());
        assert!(empty.default_api_key_env().is_none());
    }

    // ── Preset resolution ───────────────────────────────────────────────────

    #[test]
    fn preset_resolves_endpoint() {
        let mut providers = vec![ProviderConfig {
            preset: Some("openrouter".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("https://openrouter.ai/api/v1/chat/completions")
        );
    }

    #[test]
    fn openai_preset_uses_responses_protocol() {
        let mut providers = vec![ProviderConfig {
            preset: Some("openai".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].api, Some(ApiProtocol::Responses));
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
    }

    #[test]
    fn compatible_presets_keep_chat_completions() {
        for preset in [
            "lm-studio",
            "ollama",
            "openrouter",
            "groq",
            "zai",
            "zai-coding",
        ] {
            let mut providers = vec![ProviderConfig {
                preset: Some(preset.to_owned()),
                ..Default::default()
            }];
            resolve_presets(&mut providers);
            assert_eq!(
                providers[0].api,
                Some(ApiProtocol::ChatCompletions),
                "unexpected protocol for {preset}"
            );
            assert!(
                providers[0]
                    .endpoint
                    .as_deref()
                    .is_some_and(|endpoint| endpoint.ends_with("/chat/completions")),
                "unexpected endpoint for {preset}"
            );
        }
    }

    #[test]
    fn explicit_protocol_overrides_preset() {
        let mut providers = vec![ProviderConfig {
            preset: Some("openai".to_owned()),
            api: Some(ApiProtocol::ChatCompletions),
            endpoint: Some("https://api.openai.com/v1/chat/completions".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].api, Some(ApiProtocol::ChatCompletions));
    }

    #[test]
    fn preset_resolves_name() {
        let mut providers = vec![ProviderConfig {
            preset: Some("lm-studio".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].name.as_deref(), Some("lm-studio"));
    }

    #[test]
    fn explicit_endpoint_overrides_preset() {
        let mut providers = vec![ProviderConfig {
            preset: Some("openrouter".to_owned()),
            endpoint: Some("http://custom:9999/v1/chat/completions".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("http://custom:9999/v1/chat/completions")
        );
    }

    #[test]
    fn explicit_name_overrides_preset() {
        let mut providers = vec![ProviderConfig {
            preset: Some("lm-studio".to_owned()),
            name: Some("my-local".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].name.as_deref(), Some("my-local"));
    }

    #[test]
    fn unknown_preset_leaves_endpoint_none() {
        let mut providers = vec![ProviderConfig {
            preset: Some("nonexistent".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert!(providers[0].endpoint.is_none());
    }

    #[test]
    fn no_preset_unchanged() {
        let mut providers = vec![ProviderConfig {
            name: Some("custom".to_owned()),
            endpoint: Some("http://custom:1234/v1".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].name.as_deref(), Some("custom"));
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("http://custom:1234/v1")
        );
    }

    #[test]
    fn preset_zai_coding_resolves_endpoint_and_name() {
        let mut providers = vec![ProviderConfig {
            preset: Some("zai-coding".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(providers[0].name.as_deref(), Some("zai"));
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("https://api.z.ai/api/coding/paas/v4/chat/completions")
        );
    }

    #[test]
    fn preset_zai_coding_leaves_models_endpoint_none() {
        // models_endpoint is derived from the chat endpoint at request time,
        // so the coding preset must NOT fill it.
        let mut providers = vec![ProviderConfig {
            preset: Some("zai-coding".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert!(providers[0].models_endpoint.is_none());
    }

    #[test]
    fn preset_models_endpoint_skipped_when_endpoint_overridden() {
        // The user's actual config: platform `zai` preset but the Coding Plan
        // endpoint. The preset's `/api/v1/models` must NOT be applied — it
        // would 401 with a Coding-Plan key. It stays None so the models URL is
        // derived from the coding chat endpoint instead.
        let mut providers = vec![ProviderConfig {
            preset: Some("zai".to_owned()),
            endpoint: Some("https://api.z.ai/api/coding/paas/v4/chat/completions".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(
            providers[0].endpoint.as_deref(),
            Some("https://api.z.ai/api/coding/paas/v4/chat/completions")
        );
        assert!(providers[0].models_endpoint.is_none());
    }

    #[test]
    fn preset_models_endpoint_applied_when_endpoint_inherited() {
        // Standard zai (no endpoint override) still gets the preset's
        // /api/v1/models models endpoint.
        let mut providers = vec![ProviderConfig {
            preset: Some("zai".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(
            providers[0].models_endpoint.as_deref(),
            Some("https://api.z.ai/api/v1/models")
        );
    }

    #[test]
    fn preset_models_endpoint_applied_when_endpoint_equals_preset() {
        // An explicit endpoint that matches the preset exactly is not treated
        // as an override — the preset's models_endpoint still applies.
        let mut providers = vec![ProviderConfig {
            preset: Some("zai".to_owned()),
            endpoint: Some("https://api.z.ai/api/paas/v4/chat/completions".to_owned()),
            ..Default::default()
        }];
        resolve_presets(&mut providers);
        assert_eq!(
            providers[0].models_endpoint.as_deref(),
            Some("https://api.z.ai/api/v1/models")
        );
    }

    #[test]
    fn preset_after_merge_preserves_project_override() {
        // User sets preset = "openrouter", project overrides api_key_env
        // but keeps the preset so it resolves after merge.
        let user_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "openrouter"
preset = "openrouter"
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[[providers]]
name = "openrouter"
preset = "openrouter"
api_key_env = "MY_PROJECT_KEY"
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));
        let p = config.provider.default_provider().unwrap();
        // Preset resolved the endpoint.
        assert_eq!(
            p.endpoint.as_deref(),
            Some("https://openrouter.ai/api/v1/chat/completions")
        );
        // Project's api_key_env overrides user's (user had none, project set it).
        assert_eq!(p.api_key_env.as_deref(), Some("MY_PROJECT_KEY"));
        // Name preserved from merge.
        assert_eq!(p.name.as_deref(), Some("openrouter"));
    }

    #[test]
    fn preset_toml_deserialize() {
        let config: ProviderConfig = toml::from_str(
            r#"
preset = "ollama"
api_key_env = "OLLAMA_KEY"
"#,
        )
        .unwrap();
        assert_eq!(config.preset.as_deref(), Some("ollama"));
        assert_eq!(config.api_key_env.as_deref(), Some("OLLAMA_KEY"));
    }

    #[test]
    fn provider_api_protocol_deserializes_and_defaults_safely() {
        let missing: ProviderConfig = toml::from_str("name = \"custom\"").unwrap();
        assert_eq!(
            missing.api.unwrap_or_default(),
            ApiProtocol::ChatCompletions
        );

        let responses: ProviderConfig = toml::from_str("api = \"responses\"").unwrap();
        assert_eq!(responses.api, Some(ApiProtocol::Responses));

        let chat: ProviderConfig = toml::from_str("api = \"chat_completions\"").unwrap();
        assert_eq!(chat.api, Some(ApiProtocol::ChatCompletions));
    }

    #[test]
    fn provider_config_name_field_optional() {
        // Deserialize with name.
        let with_name: ProviderConfig = toml::from_str(
            r#"
name = "openrouter"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
"#,
        )
        .unwrap();
        assert_eq!(with_name.name.as_deref(), Some("openrouter"));

        // Deserialize without name.
        let without_name: ProviderConfig = toml::from_str(
            r#"
endpoint = "http://localhost:1234/v1/chat/completions"
"#,
        )
        .unwrap();
        assert!(without_name.name.is_none());
    }

    // ── Extension config ───────────────────────────────────────────────────

    #[test]
    fn extension_config_defaults() {
        let config = ExtensionConfig::default();
        assert!(config.enabled.is_empty());
        assert!(config.disabled.is_empty());
        assert!(config.defaults.network.is_none());
        assert!(config.defaults.commands.is_none());
        assert!(config.defaults.max_memory_mb.is_none());
        assert!(config.defaults.max_execution_time_s.is_none());
        assert!(config.per_extension.is_empty());
    }

    #[test]
    fn extension_is_enabled_empty_lists_allows_all() {
        let config = ExtensionConfig::default();
        assert!(config.is_enabled("anything"));
        assert!(config.is_enabled("other"));
    }

    #[test]
    fn extension_is_enabled_allowlist_filters() {
        let config = ExtensionConfig {
            enabled: vec!["crates-search".into(), "rust-docs".into()],
            ..Default::default()
        };
        assert!(config.is_enabled("crates-search"));
        assert!(config.is_enabled("rust-docs"));
        assert!(!config.is_enabled("unknown"));
    }

    #[test]
    fn extension_is_enabled_denylist_overrides_allowlist() {
        let config = ExtensionConfig {
            enabled: vec!["crates-search".into(), "rust-docs".into()],
            disabled: vec!["rust-docs".into()],
            ..Default::default()
        };
        assert!(config.is_enabled("crates-search"));
        assert!(!config.is_enabled("rust-docs"));
    }

    #[test]
    fn extension_is_enabled_denylist_without_allowlist() {
        let config = ExtensionConfig {
            disabled: vec!["experimental".into()],
            ..Default::default()
        };
        assert!(!config.is_enabled("experimental"));
        assert!(config.is_enabled("everything-else"));
    }

    #[test]
    fn extension_permissions_for_defaults_only() {
        let config = ExtensionConfig {
            defaults: ExtensionPermissions {
                network: Some(true),
                max_memory_mb: Some(128),
                ..Default::default()
            },
            ..Default::default()
        };

        let perms = config.permissions_for("my-extension");
        assert_eq!(perms.network, Some(true));
        assert_eq!(perms.max_memory_mb, Some(128));
        assert!(perms.commands.is_none());
        assert!(perms.max_execution_time_s.is_none());
    }

    #[test]
    fn extension_permissions_per_extension_override() {
        let config = ExtensionConfig {
            defaults: ExtensionPermissions {
                network: Some(true),
                max_memory_mb: Some(64),
                ..Default::default()
            },
            per_extension: {
                let mut map = std::collections::HashMap::new();
                map.insert(
                    "rust-docs".into(),
                    ExtensionPermissions {
                        max_memory_mb: Some(256),
                        ..Default::default()
                    },
                );
                map
            },
            ..Default::default()
        };

        // rust-docs gets per_extension override for max_memory_mb, inherits defaults for network
        let perms = config.permissions_for("rust-docs");
        assert_eq!(perms.network, Some(true));
        assert_eq!(perms.max_memory_mb, Some(256));

        // unknown extension gets only defaults
        let perms = config.permissions_for("unknown");
        assert_eq!(perms.network, Some(true));
        assert_eq!(perms.max_memory_mb, Some(64));
    }

    #[test]
    fn extension_config_from_toml() {
        let dir = TempDir::new().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_path_buf())),
                ("USERPROFILE", Some(dir.path().to_path_buf())),
                ("XDG_CONFIG_HOME", Some(dir.path().to_path_buf())),
            ],
            || {
                let rho_dir = dir.path().join(".rho");
                std::fs::create_dir_all(&rho_dir).unwrap();

                std::fs::write(
                    rho_dir.join("config.toml"),
                    r#"
[extensions]
enabled = ["crates-search", "rust-docs"]
disabled = ["experimental"]

[extensions.defaults]
network = true
max_memory_mb = 64

[extensions.per_extension."rust-docs"]
max_memory_mb = 128
"#,
                )
                .unwrap();

                let config = ConfigLoader::load(dir.path()).unwrap();

                assert_eq!(
                    config.extensions.enabled,
                    vec!["crates-search", "rust-docs"]
                );
                assert_eq!(config.extensions.disabled, vec!["experimental"]);
                assert_eq!(config.extensions.defaults.network, Some(true));
                assert_eq!(config.extensions.defaults.max_memory_mb, Some(64));

                let perms = config.extensions.permissions_for("rust-docs");
                assert_eq!(perms.max_memory_mb, Some(128));
                assert_eq!(perms.network, Some(true)); // inherited from defaults
            },
        );
    }

    #[test]
    fn extension_config_project_overrides_user() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[extensions]
enabled = ["crates-search", "rust-docs", "experimental"]

[extensions.defaults]
network = false
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[extensions]
enabled = ["crates-search", "rust-docs"]
disabled = ["experimental"]

[extensions.defaults]
network = true
max_memory_mb = 128
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));

        // Project enabled replaces user enabled (Vec replacement semantics)
        assert_eq!(
            config.extensions.enabled,
            vec!["crates-search", "rust-docs"]
        );
        // Project disabled replaces user disabled
        assert_eq!(config.extensions.disabled, vec!["experimental"]);
        // Project defaults override user defaults
        assert_eq!(config.extensions.defaults.network, Some(true));
        assert_eq!(config.extensions.defaults.max_memory_mb, Some(128));
    }

    #[test]
    fn extension_config_user_preserved_when_no_project() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[extensions]
enabled = ["crates-search"]

[extensions.defaults]
network = true
"#,
        )
        .unwrap();

        let config = ConfigLoader::merge(Some(user_wire), None);
        assert_eq!(config.extensions.enabled, vec!["crates-search"]);
        assert_eq!(config.extensions.defaults.network, Some(true));
    }

    #[test]
    fn models_section_parses_user_pricing() {
        let wire: WireConfig = toml::from_str(
            r#"
[[models]]
id = "llama-3.1-8b-instant"
provider = "groq"
input_price = 0.05
output_price = 0.08
cached_price = 0.01

[[models]]
id = "my-self-hosted"
input_price = 0.0
output_price = 0.0
"#,
        )
        .unwrap();
        let config = ConfigLoader::merge(Some(wire), None);
        assert_eq!(config.models.len(), 2);
        let groq = config
            .models
            .iter()
            .find(|m| m.id == "llama-3.1-8b-instant")
            .expect("groq model present");
        assert_eq!(groq.provider.as_deref(), Some("groq"));
        assert!((groq.input_price - 0.05).abs() < 1e-9);
        assert_eq!(groq.cached_price, Some(0.01));
        let self_hosted = config
            .models
            .iter()
            .find(|m| m.id == "my-self-hosted")
            .expect("self-hosted model present");
        assert!(self_hosted.provider.is_none());
        assert!(self_hosted.cached_price.is_none());
    }

    #[test]
    fn models_project_overrides_user_by_id() {
        let user_wire: WireConfig = toml::from_str(
            r#"
[[models]]
id = "m"
input_price = 1.0
output_price = 2.0
"#,
        )
        .unwrap();
        let project_wire: WireConfig = toml::from_str(
            r#"
[[models]]
id = "m"
input_price = 9.0
output_price = 9.0
"#,
        )
        .unwrap();
        let config = ConfigLoader::merge(Some(user_wire), Some(project_wire));
        assert_eq!(config.models.len(), 1, "project overrides user by id");
        let m = config.models.iter().find(|m| m.id == "m").unwrap();
        assert!((m.input_price - 9.0).abs() < 1e-9);
    }

    #[test]
    fn user_model_config_to_catalog_model_carries_pricing() {
        let cfg = UserModelConfig {
            id: "x".into(),
            provider: Some("p".into()),
            input_price: 3.0,
            output_price: 4.0,
            cached_price: Some(0.5),
        };
        let m = cfg.to_catalog_model();
        assert_eq!(m.id, "x");
        assert_eq!(m.provider, "p");
        assert!((m.cost.input - 3.0).abs() < 1e-9);
        assert!((m.cost.output - 4.0).abs() < 1e-9);
        assert!((m.cost.cache_read - 0.5).abs() < 1e-9);
        // Priced (not sentinel) → cost_for yields a usable value.
        let usage = rho_ai::StreamUsage::new(1_000_000, 0);
        assert!(m.cost_for(&usage).is_some());
    }
}
