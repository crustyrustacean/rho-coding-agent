//! Individual task implementations.

use std::process::Command;

use anyhow::{Context, Result, bail};

/// Workspace root directory.
fn workspace_root() -> &'static str {
    // At compile time, CARGO_MANIFEST_DIR is `<root>/xtask`.
    // Strip the suffix to get the workspace root.
    const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
    &MANIFEST_DIR[..MANIFEST_DIR.len() - "/xtask".len()]
}

/// Spawn a command, inheriting stdout/stderr, and return an error on non-zero exit.
fn spawn(label: &str, cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .with_context(|| format!("{label}: failed to spawn `{cmd}`"))?;

    if status.success() {
        Ok(())
    } else {
        bail!("{label}: `{cmd}` exited with {status}")
    }
}

/// `cargo xtask fmt` — check formatting without modifying files.
pub fn fmt() -> Result<()> {
    spawn("fmt", "cargo", &["fmt", "--all", "--", "--check"])
}

/// `cargo xtask fmt-fix` — apply formatting in place.
pub fn fmt_fix() -> Result<()> {
    spawn("fmt-fix", "cargo", &["fmt", "--all"])
}

/// `cargo xtask lint` — run Clippy on all targets.
pub fn lint() -> Result<()> {
    spawn(
        "lint",
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

/// `cargo xtask build [--release]` — build all workspace crates.
pub fn build(release: bool) -> Result<()> {
    let mut args = vec!["build", "--workspace"];
    if release {
        args.push("--release");
    }
    spawn("build", "cargo", &args)
}

/// `cargo xtask test [--release] [-- <args>...]` — run all tests.
pub fn test(release: bool, extra_args: &[String]) -> Result<()> {
    let mut args: Vec<&str> = vec!["test", "--workspace"];
    if release {
        args.push("--release");
    }
    if !extra_args.is_empty() {
        args.push("--");
        args.extend(extra_args.iter().map(String::as_str));
    }
    spawn("test", "cargo", &args)
}

/// `cargo xtask run [-- <args>...]` — run the main binary.
pub fn run(extra_args: &[String]) -> Result<()> {
    let mut args: Vec<&str> = vec!["run", "-p", "rho-core"];
    if !extra_args.is_empty() {
        args.push("--");
        args.extend(extra_args.iter().map(String::as_str));
    }
    spawn("run", "cargo", &args)
}

/// `cargo xtask clean` — remove build artifacts.
pub fn clean() -> Result<()> {
    spawn("clean", "cargo", &["clean"])
}

/// `cargo xtask ci` — the full CI pipeline (fmt, lint, build, test).
pub fn ci() -> Result<()> {
    fmt().context("fmt")?;
    lint().context("lint")?;
    build(false).context("build")?;
    test(false, &[]).context("test")?;
    Ok(())
}

/// `cargo xtask changelog [VERSION]` — generate/update CHANGELOG.md.
pub fn changelog(version: Option<&str>) -> Result<()> {
    match version {
        Some(v) => spawn(
            "changelog",
            "git",
            &["cliff", "-o", "CHANGELOG.md", "--tag", v],
        ),
        None => spawn("changelog", "git", &["cliff", "-o", "CHANGELOG.md"]),
    }
}

/// `cargo xtask status` — show a workspace summary.
pub fn status() -> Result<()> {
    let root = workspace_root();
    println!("📦 Workspace: {root}");

    let output = Command::new("cargo")
        .args(["--version"])
        .current_dir(root)
        .output()
        .context("status: failed to run cargo --version")?;
    println!("⚙️  {}", String::from_utf8_lossy(&output.stdout).trim());

    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(root)
        .output()
        .context("status: failed to run git branch")?;
    println!(
        "🌿 branch: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );

    let output = Command::new("git")
        .args(["log", "--oneline", "-5"])
        .current_dir(root)
        .output()
        .context("status: failed to run git log")?;
    println!("📜 recent commits:");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        println!("   {line}");
    }

    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(root)
        .output()
        .context("status: failed to run git status")?;
    let status = String::from_utf8_lossy(&output.stdout);
    if status.is_empty() {
        println!("✅ working tree clean");
    } else {
        println!("⚠️  uncommitted changes:");
        for line in status.lines() {
            println!("   {line}");
        }
    }

    Ok(())
}
