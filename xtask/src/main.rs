//! Development task runner (`cargo xtask`).

use anyhow::Result;
use clap::Parser;

mod tasks;

/// rho-coding-agent development task runner.
#[derive(Parser)]
#[command(bin_name = "cargo xtask", subcommand_required = true)]
enum Xtask {
    /// Check formatting without modifying files.
    Fmt,

    /// Apply formatting in place.
    #[command(name = "fmt-fix")]
    FmtFix,

    /// Run Clippy lints.
    Lint,

    /// Build all workspace crates.
    Build {
        /// Build with release optimizations.
        #[arg(long)]
        release: bool,
    },

    /// Run all workspace tests.
    Test {
        /// Run tests with release optimizations.
        #[arg(long)]
        release: bool,
        /// Pass extra arguments to the test binary (e.g. `-- --nocapture`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run the main binary.
    Run {
        /// Pass extra arguments to the binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Remove build artifacts.
    Clean,

    /// Run the full CI pipeline (fmt, lint, build, test).
    Ci,

    /// Generate or update CHANGELOG.md via git-cliff.
    Changelog {
        /// Target version tag (e.g. "0.2.0"). Omit for unreleased changes.
        version: Option<String>,
    },

    /// Show workspace status summary.
    Status,
}

fn main() -> Result<()> {
    match Xtask::parse() {
        Xtask::Fmt => tasks::fmt(),
        Xtask::FmtFix => tasks::fmt_fix(),
        Xtask::Lint => tasks::lint(),
        Xtask::Build { release } => tasks::build(release),
        Xtask::Test { release, args } => tasks::test(release, &args),
        Xtask::Run { args } => tasks::run(&args),
        Xtask::Clean => tasks::clean(),
        Xtask::Ci => tasks::ci(),
        Xtask::Changelog { version } => tasks::changelog(version.as_deref()),
        Xtask::Status => tasks::status(),
    }
}
