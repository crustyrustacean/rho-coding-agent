//! CLI argument parsing for `rho`.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// rho — a local coding agent (headless RPC mode).
#[derive(Debug, Parser)]
#[command(version, about)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    /// Model identifier.
    ///
    /// If omitted (and not set in config), rho uses the first provider's
    /// `default_model` if configured, or reports an error.
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

    /// Optional subcommand. When absent, rho runs as the headless agent.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands for `rho`.
///
/// When `Cli::command` is `None`, rho runs as the headless agent with the
/// flat CLI flags. When a subcommand is present, it is dispatched
/// independently of the agent.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Manage extensions (install, sync, remove, list).
    Extension(ExtensionArgs),
}

/// Arguments for the `rho extension` subcommand.
#[derive(Debug, Clone, Args)]
pub struct ExtensionArgs {
    /// The extension management command to execute.
    #[command(subcommand)]
    pub command: ExtensionCommand,
}

/// Extension management commands.
#[derive(Debug, Clone, Subcommand)]
pub enum ExtensionCommand {
    /// Sync extensions from the manifest file.
    Sync,
    /// Install an extension from a URL.
    Install {
        /// URL or file path of the extension to install.
        url: String,
    },
    /// Remove an installed extension.
    Remove {
        /// Name of the extension to remove.
        name: String,
    },
    /// List installed extensions.
    List,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("rho").chain(args.iter().copied()))
    }

    #[test]
    fn default_parse() {
        let cli = parse(&[]).expect("default parse");
        assert!(cli.model.is_none());
        assert!(!cli.ephemeral);
    }

    #[test]
    fn ephemeral_flag() {
        let cli = parse(&["--ephemeral"]).expect("--ephemeral");
        assert!(cli.ephemeral);
    }

    #[test]
    fn model_flag() {
        let cli = parse(&["--model", "gpt-4o"]).expect("--model");
        assert_eq!(cli.model.as_deref(), Some("gpt-4o"));
    }

    // ── Subcommand parsing tests ───────���────────────────────────

    #[test]
    fn extension_sync_parses() {
        let cli = parse(&["extension", "sync"]).expect("extension sync");
        assert!(cli.command.is_some());
        let Command::Extension(args) = cli.command.unwrap();
        assert!(matches!(args.command, ExtensionCommand::Sync));
    }

    #[test]
    fn extension_install_parses() {
        let cli = parse(&["extension", "install", "https://example.com/hello.ts"])
            .expect("extension install");
        let Command::Extension(args) = cli.command.unwrap();
        let ExtensionCommand::Install { url } = args.command else {
            panic!("expected Install command");
        };
        assert_eq!(url, "https://example.com/hello.ts");
    }

    #[test]
    fn extension_remove_parses() {
        let cli = parse(&["extension", "remove", "hello"]).expect("extension remove");
        let Command::Extension(args) = cli.command.unwrap();
        let ExtensionCommand::Remove { name } = args.command else {
            panic!("expected Remove command");
        };
        assert_eq!(name, "hello");
    }

    #[test]
    fn extension_list_parses() {
        let cli = parse(&["extension", "list"]).expect("extension list");
        let Command::Extension(args) = cli.command.unwrap();
        assert!(matches!(args.command, ExtensionCommand::List));
    }

    #[test]
    fn no_subcommand_yields_none() {
        let cli = parse(&[]).expect("default parse");
        assert!(cli.command.is_none());
    }

    #[test]
    fn flat_flags_unchanged_with_no_subcommand() {
        let cli = parse(&["--model", "gpt-4o", "--ephemeral"]).expect("flags");
        assert!(cli.command.is_none());
        assert_eq!(cli.model.as_deref(), Some("gpt-4o"));
        assert!(cli.ephemeral);
    }
}
