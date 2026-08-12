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
    ///
    /// Uses `--prepend` so existing entries are preserved.
    /// Reads the current version from workspace Cargo.toml.
    Changelog,

    /// Prepare a release: bump version, update changelog, tag, and commit.
    ///
    /// Steps:
    ///   1. Run CI to ensure a clean tree (unless --skip-ci).
    ///   2. Bump the version in workspace `Cargo.toml`.
    ///   3. Generate a changelog entry via git-cliff and prepend to CHANGELOG.md.
    ///   4. Commit everything with `chore(release): prepare <version>`.
    ///   5. Create a `v<version>` git tag on the release commit.
    ///
    /// Refuses to run if there are no commits since the last tag (would produce
    /// an empty release); pass --allow-empty to override.
    ///
    /// The tag and commit are local only — push with `git push origin trunk --tags`.
    Release {
        /// The new version (e.g. "0.48.0", "major", "minor", "patch").
        version: String,

        /// Skip the CI check (use if you just ran `cargo xtask ci`).
        #[arg(long)]
        skip_ci: bool,

        /// Release even when there are no commits since the last tag
        /// (otherwise the empty-release guard refuses to run).
        #[arg(long)]
        allow_empty: bool,
    },

    /// Show workspace status summary.
    Status,

    /// Generate/update the `OpenRPC` schema in `docs/rpc-schema/openrpc.json`.
    Schema,

    /// Generate the built-in model catalog from `OpenRouter`'s API.
    ///
    /// Fetches models from `https://openrouter.ai/api/v1/models`, applies
    /// manual overrides from `rho-ai/model-overrides.json`, and writes
    /// `rho-ai/src/catalog_generated.rs`.
    GenerateModels,
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
        Xtask::Changelog => tasks::changelog(),
        Xtask::Release {
            version,
            skip_ci,
            allow_empty,
        } => tasks::release(&version, skip_ci, allow_empty),
        Xtask::Status => tasks::status(),
        Xtask::Schema => tasks::schema(),
        Xtask::GenerateModels => tasks::generate_models(),
    }
}
