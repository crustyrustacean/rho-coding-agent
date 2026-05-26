//! CLI argument parsing for `rho`.

use clap::Parser;
use std::path::PathBuf;

/// rho — a local coding agent.
#[derive(Debug, Parser)]
#[command(version, about)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    /// Model identifier.
    ///
    /// If omitted (and not set in config), rho queries the server's
    /// `/v1/models` endpoint and uses the first loaded model.
    #[arg(short, long)]
    pub model: Option<String>,

    /// System prompt (overrides the bundled base prompt and context files).
    #[arg(short, long)]
    pub system: Option<String>,

    /// Use a compact system prompt suitable for models with small context
    /// windows (e.g. 4K tokens). The full prompt (~2,000 tokens) plus tool
    /// schemas and context files may exceed the context length of smaller
    /// models. This flag swaps the full prompt for a minimal version (~100
    /// tokens) that preserves core identity and safety rules.
    #[arg(long)]
    pub compact: bool,

    /// Project root / sandbox root (defaults to auto-detected project root).
    ///
    /// When omitted, rho walks up from the current directory looking for
    /// project markers (`.rho/config.toml`, `.git/`, `Cargo.toml`, etc.).
    /// Falls back to the current directory if no marker is found.
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Model API endpoint URL.
    ///
    /// Overrides the `[provider] endpoint` config value.
    /// When set to a non-local host, the external provider consent
    /// warning is automatically skipped (equivalent to
    /// `--accept-external-provider`).
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Skip the provider consent warning for external endpoints.
    ///
    /// By default, rho displays a consent prompt before connecting to a
    /// non-local model provider. Use this flag to skip the prompt in
    /// automated workflows where consent has been pre-authorized.
    #[arg(long)]
    pub accept_external_provider: bool,

    /// Environment variable containing the API key for bearer authentication.
    ///
    /// Overrides the `[provider] api_key_env` config value.
    /// Ignored for local endpoints.
    #[arg(long)]
    pub api_key_env: Option<String>,

    /// Maximum agent loop iterations before terminating.
    ///
    /// Overrides the `[agent] max_iterations` config value.
    /// Defaults to 32.
    #[arg(long)]
    pub max_iterations: Option<u32>,

    /// Context window token budget.
    ///
    /// Controls how many tokens the sliding window retains before evicting
    /// older turns. Overrides the `[agent] token_budget` config value.
    /// Defaults to 32,768.
    #[arg(long)]
    pub token_budget: Option<u32>,

    /// Resume the most recent session for this project.
    ///
    /// Scans `~/.rho/sessions/` for the latest JSONL file matching the
    /// current project directory and resumes it. Equivalent to `--session
    /// <path>` but finds the path automatically.
    ///
    /// Exits with an error if no previous sessions exist.
    #[arg(short, long, conflicts_with_all = ["session", "ephemeral"])]
    pub r#continue: bool,

    /// Resume a previous session from a JSONL file.
    ///
    /// When specified, rho loads the session from the given path instead of
    /// creating a new one. Use this to continue a conversation that was
    /// interrupted or to inspect a session's history.
    #[arg(long, conflicts_with_all = ["continue", "ephemeral"])]
    pub session: Option<PathBuf>,

    /// Run in ephemeral mode — no session file is written to disk.
    ///
    /// All conversation state lives only in memory and is lost when rho
    /// exits. Useful for one-shot commands, CI pipelines, or when you
    /// don't want `.rho/sessions/` clutter.
    #[arg(long, conflicts_with_all = ["continue", "session"])]
    pub ephemeral: bool,
}
