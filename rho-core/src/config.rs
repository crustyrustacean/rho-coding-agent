//! Configuration loading and merging.
//!
//! rho reads configuration from two TOML files, merged with project-level
//! overrides taking precedence over user-level defaults:
//!
//! | Source | Path | Purpose |
//! |---|---|---|
//! | User-level | `~/.rho/config.toml` | Global defaults: default model, API endpoint, provider, egress allowlist |
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
//! For `Vec` fields (denylist commands, egress hosts, context scan list):
//! the project-level list **replaces** the user-level list, it does not
//! append. This avoids surprising composition effects and keeps overrides
//! predictable.

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
    /// Model provider settings.
    pub provider: ProviderConfig,
    /// Per-tool approval policies.
    pub approval: ApprovalConfig,
    /// Shell command safety settings.
    pub shell: ShellConfig,
    /// File sandbox settings.
    pub sandbox: SandboxConfig,
    /// Project context file settings.
    pub context: ContextConfig,
    /// Network egress control.
    pub egress: EgressConfig,
    /// Secret redaction settings.
    pub redaction: RedactionConfig,
    /// System prompt extensions.
    pub system_prompt: SystemPromptConfig,
}

// ── AgentLoopConfig ───────────────────────────────────────────────────────────

/// Agent loop tuning parameters.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentLoopConfig {
    /// Model identifier (e.g. `"qwen3-8b"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum model-tool-model round trips before the loop fails.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Maximum retries on transient errors before giving up.
    #[serde(default = "default_retry_budget")]
    pub retry_budget: u32,
    /// Base backoff in milliseconds; doubles on each retry (capped at 64×).
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            model: None,
            max_iterations: default_max_iterations(),
            retry_budget: default_retry_budget(),
            initial_backoff_ms: default_initial_backoff_ms(),
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

// ── ProviderConfig ────────────────────────────────────────────────────────────

/// Model provider selection and connection settings.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// Provider type: `"local"` (default), `"openai"`, `"anthropic"`, etc.
    #[serde(default)]
    pub r#type: Option<String>,
    /// API endpoint URL.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Environment variable name holding the API key.
    ///
    /// The key itself is never stored in config; this field names the env var
    /// to read at runtime.
    #[serde(default)]
    pub api_key_env: Option<String>,
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

// ── SandboxConfig ─────────────────────────────────────────────────────────────

/// File sandbox settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SandboxConfig {
    /// Whether the file sandbox is enabled. Defaults to `true`.
    /// Disabling is **not recommended** — it removes path traversal protection.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
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

// ── EgressConfig ──────────────────────────────────────────────────────────────

/// Network egress control.
///
/// The egress allowlist controls which hosts the agent is permitted to contact.
/// `LocalChatClient` defaults to `localhost` only. External providers add
/// their API hostname to this list.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EgressConfig {
    /// Hostnames the agent is allowed to contact (in addition to `localhost`).
    ///
    /// `localhost` is always allowed and does not need to be listed.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

// ── RedactionConfig ───────────────────────────────────────────────────────────

/// Secret redaction settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedactionConfig {
    /// Whether secret redaction is enabled. Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
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

// ── Wire format (TOML-deserializable) ─────────────────────────────────────────

/// The TOML wire format for a config file.
///
/// All fields are optional so partial configs work naturally. Missing fields
/// fall through to defaults or to the other config tier.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireConfig {
    /// Agent loop settings.
    #[serde(default)]
    agent: Option<WireAgentLoopConfig>,
    /// Provider settings.
    #[serde(default)]
    provider: Option<ProviderConfig>,
    /// Approval settings.
    #[serde(default)]
    approval: Option<ApprovalConfig>,
    /// Shell settings.
    #[serde(default)]
    shell: Option<ShellConfig>,
    /// Sandbox settings.
    #[serde(default)]
    sandbox: Option<SandboxConfig>,
    /// Context file settings.
    #[serde(default)]
    context: Option<ContextConfig>,
    /// Egress settings.
    #[serde(default)]
    egress: Option<EgressConfig>,
    /// Redaction settings.
    #[serde(default)]
    redaction: Option<RedactionConfig>,
    /// System prompt settings.
    #[serde(default)]
    system_prompt: Option<SystemPromptConfig>,
}

/// Wire format for `[agent]` section.
#[derive(Clone, Debug, Default, Deserialize)]
struct WireAgentLoopConfig {
    /// Model identifier.
    #[serde(default)]
    model: Option<String>,
    /// Max iterations before loop fails.
    #[serde(default)]
    max_iterations: Option<u32>,
    /// Retry budget for transient errors.
    #[serde(default)]
    retry_budget: Option<u32>,
    /// Base backoff in milliseconds.
    #[serde(default)]
    initial_backoff_ms: Option<u64>,
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
    fn merge(user: Option<WireConfig>, project: Option<WireConfig>) -> RhoConfig {
        let user = user.unwrap_or_default();
        let project = project.unwrap_or_default();

        let user_agent = user.agent.unwrap_or_default();
        let project_agent = project.agent.unwrap_or_default();

        RhoConfig {
            agent: AgentLoopConfig {
                model: project_agent.model.or(user_agent.model),
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
            },
            provider: {
                let up = user.provider.unwrap_or_default();
                let pp = project.provider.unwrap_or_default();
                ProviderConfig {
                    r#type: pp.r#type.or(up.r#type),
                    endpoint: pp.endpoint.or(up.endpoint),
                    api_key_env: pp.api_key_env.or(up.api_key_env),
                }
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
            sandbox: project.sandbox.or(user.sandbox).unwrap_or_default(),
            context: project.context.or(user.context).unwrap_or_default(),
            egress: project.egress.or(user.egress).unwrap_or_default(),
            redaction: project.redaction.or(user.redaction).unwrap_or_default(),
            system_prompt: project
                .system_prompt
                .or(user.system_prompt)
                .unwrap_or_default(),
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
    /// Returns `None` if `provider.api_key_env` is not set or the env var
    /// doesn't exist. Returns `Some(key)` if the env var is set.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.provider
            .api_key_env
            .as_ref()
            .and_then(|var| std::env::var(var).ok())
    }

    /// Check whether a hostname is permitted by the egress allowlist.
    ///
    /// `localhost` is always allowed. Other hosts must appear in
    /// `egress.allowed_hosts`.
    pub fn is_host_allowed(&self, host: &str) -> bool {
        if host == "localhost" || host == "127.0.0.1" || host == "::1" {
            return true;
        }
        self.egress.allowed_hosts.iter().any(|h| h == host)
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
        let config = ConfigLoader::load(dir.path()).unwrap();

        assert!(config.agent.model.is_none());
        assert_eq!(config.agent.max_iterations, 32);
        assert_eq!(config.agent.retry_budget, 4);
        assert_eq!(config.agent.initial_backoff_ms, 500);
        assert!(config.provider.r#type.is_none());
        assert!(config.provider.endpoint.is_none());
        assert!(config.provider.api_key_env.is_none());
        assert!(config.approval.per_tool.is_empty());
        assert!(config.shell.denied_commands.is_empty());
        assert!(config.sandbox.enabled);
        assert!(config.context.scan_list.is_none());
        assert!(config.egress.allowed_hosts.is_empty());
        assert!(config.redaction.enabled);
        assert!(config.system_prompt.extensions.is_empty());
    }

    #[test]
    fn load_project_config_overrides_defaults() {
        let dir = TempDir::new().unwrap();
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

[sandbox]
enabled = true

[context]
scan_list = ["AGENTS.md", "CLAUDE.md"]

[egress]
allowed_hosts = ["api.openai.com"]

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
        assert_eq!(config.provider.r#type.as_deref(), Some("openai"));
        assert_eq!(
            config.provider.endpoint.as_deref(),
            Some("https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(
            config.provider.api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
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
        assert!(config.sandbox.enabled);
        assert_eq!(
            config.context.scan_list,
            Some(vec!["AGENTS.md".to_owned(), "CLAUDE.md".to_owned()])
        );
        assert_eq!(config.egress.allowed_hosts, vec!["api.openai.com"]);
        assert!(!config.redaction.enabled);
        assert_eq!(
            config.system_prompt.extensions,
            vec!["Always use PowerShell 7."]
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
        // User provider settings are preserved (project didn't set them).
        assert_eq!(config.provider.r#type.as_deref(), Some("local"));
        assert_eq!(
            config.provider.endpoint.as_deref(),
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

        // Allow unknown fields by using a permissive parse.
        // Our WireConfig uses Option fields, so unknown top-level tables
        // will cause a parse error. We need to decide: silently ignore or error?
        // For forward compat, we should ignore unknown keys.
        // toml::from_str does this by default with serde(default).
        // But unknown tables will error. Let's test the current behavior.
        let result = ConfigLoader::load(dir.path());
        // Unknown top-level keys cause an error with strict TOML parsing.
        // This is acceptable — we can add `#[serde(deny_unknown_fields)]` later
        // or switch to a more lenient approach. For now, unknown tables error.
        // Actually, serde+toml by default ignores unknown fields.
        // Let's just verify it works:
        assert!(result.is_ok(), "unknown scalar fields should be ignored");
    }

    // ── Egress allowlist ───────────────────────────────────────────────────

    #[test]
    fn localhost_always_allowed() {
        let config = RhoConfig::default();
        assert!(config.is_host_allowed("localhost"));
        assert!(config.is_host_allowed("127.0.0.1"));
        assert!(config.is_host_allowed("::1"));
    }

    #[test]
    fn unknown_host_denied_by_default() {
        let config = RhoConfig::default();
        assert!(!config.is_host_allowed("api.openai.com"));
    }

    #[test]
    fn allowed_hosts_permitted() {
        let config = RhoConfig {
            egress: EgressConfig {
                allowed_hosts: vec!["api.openai.com".to_owned()],
            },
            ..Default::default()
        };
        assert!(config.is_host_allowed("api.openai.com"));
        assert!(!config.is_host_allowed("api.anthropic.com"));
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
            provider: ProviderConfig {
                api_key_env: Some("RHO_TEST_NONEXISTENT_KEY_12345".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.resolve_api_key().is_none());
    }

    // ── Sandbox default ────────────────────────────────────────────────────

    #[test]
    fn sandbox_enabled_by_default() {
        let config = RhoConfig::default();
        assert!(config.sandbox.enabled);
    }

    #[test]
    fn sandbox_can_be_disabled() {
        let dir = TempDir::new().unwrap();
        let rho_dir = dir.path().join(".rho");
        std::fs::create_dir_all(&rho_dir).unwrap();
        std::fs::write(
            rho_dir.join("config.toml"),
            r"
[sandbox]
enabled = false
",
        )
        .unwrap();

        let config = ConfigLoader::load(dir.path()).unwrap();
        assert!(!config.sandbox.enabled);
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
    }

    #[test]
    fn config_with_only_provider_section() {
        let dir = TempDir::new().unwrap();
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
        assert_eq!(config.provider.r#type.as_deref(), Some("local"));
        assert_eq!(
            config.provider.endpoint.as_deref(),
            Some("http://localhost:8080/v1/chat/completions")
        );
        // Everything else is default.
        assert!(config.agent.model.is_none());
        assert!(config.sandbox.enabled);
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
}
