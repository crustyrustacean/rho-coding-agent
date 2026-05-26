//! Individual task implementations.

use std::fs;
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

/// Read the current workspace version from Cargo.toml.
fn read_workspace_version() -> Result<String> {
    let root = workspace_root();
    let cargo_toml_path = format!("{root}/Cargo.toml");
    let contents = fs::read_to_string(&cargo_toml_path)
        .with_context(|| format!("failed to read {cargo_toml_path}"))?;
    let doc: toml::Value = contents
        .parse::<toml::Value>()
        .with_context(|| format!("failed to parse {cargo_toml_path}"))?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("could not find workspace.package.version in Cargo.toml"))
}

/// Write the workspace version in Cargo.toml, replacing only the version value.
fn write_workspace_version(new_version: &str) -> Result<()> {
    let root = workspace_root();
    let cargo_toml_path = format!("{root}/Cargo.toml");
    let contents = fs::read_to_string(&cargo_toml_path)
        .with_context(|| format!("failed to read {cargo_toml_path}"))?;

    // Parse to validate it's valid TOML first.
    let _doc: toml::Value = contents
        .parse::<toml::Value>()
        .with_context(|| format!("failed to parse {cargo_toml_path}"))?;

    // Find the [workspace.package] version line and replace it.
    let mut found = false;
    let mut in_workspace_package = false;
    let new_contents: String = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed == "[workspace.package]" {
                in_workspace_package = true;
                return line.to_owned();
            }
            // Another section header ends the workspace.package scope.
            if trimmed.starts_with('[') {
                in_workspace_package = false;
            }
            if in_workspace_package
                && trimmed.starts_with("version")
                && let Some(eq_pos) = trimmed.find('=')
            {
                let key = &trimmed[..=eq_pos];
                found = true;
                return format!("{key} \"{new_version}\"");
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");

    if !found {
        bail!("could not find version = \"...\" under [workspace.package] in Cargo.toml");
    }

    // Preserve trailing newline.
    let new_contents = if contents.ends_with('\n') {
        new_contents
    } else {
        new_contents + "\n"
    };

    fs::write(&cargo_toml_path, new_contents)
        .with_context(|| format!("failed to write {cargo_toml_path}"))?;
    Ok(())
}

/// Resolve a version specifier against the current workspace version.
///
/// Accepts:
///   - An explicit semver like "0.48.0"
///   - "major", "minor", or "patch" to bump the current version
fn resolve_version(spec: &str, current: &str) -> Result<String> {
    // If it parses as semver, use it directly.
    if spec.parse::<semver::Version>().is_ok() {
        return Ok(spec.to_owned());
    }

    let mut v: semver::Version = current
        .parse()
        .with_context(|| format!("current version {current:?} is not valid semver"))?;

    match spec {
        "major" => {
            v.major += 1;
            v.minor = 0;
            v.patch = 0;
        }
        "minor" => {
            v.minor += 1;
            v.patch = 0;
        }
        "patch" => {
            v.patch += 1;
        }
        other => bail!(
            "invalid version specifier {other:?}. \
             Use a semver string (e.g. \"0.48.0\") or \"major\"/\"minor\"/\"patch\"."
        ),
    }

    Ok(v.to_string())
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

/// `cargo xtask test [--release] [-- <args>...]` — run all tests via nextest.
///
/// Falls back to `cargo test` if `cargo-nextest` is not installed.
pub fn test(release: bool, extra_args: &[String]) -> Result<()> {
    let use_nextest = which::which("cargo-nextest").is_ok();

    if use_nextest {
        let mut args: Vec<&str> = vec!["nextest", "run", "--workspace"];
        if release {
            args.push("--release");
        }
        if !extra_args.is_empty() {
            args.extend(extra_args.iter().map(String::as_str));
        }
        spawn("test", "cargo", &args)
    } else {
        eprintln!("note: cargo-nextest not found, falling back to cargo test");
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
}

/// `cargo xtask run [-- <args>...]` — run the main binary.
pub fn run(extra_args: &[String]) -> Result<()> {
    let mut args: Vec<&str> = vec!["run", "-p", "rho"];
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

/// `cargo xtask changelog` — prepend a changelog entry for the current version.
///
/// Reads the version from workspace Cargo.toml, uses `git cliff --prepend`
/// so existing entries are never destroyed.
pub fn changelog() -> Result<()> {
    let version = read_workspace_version()?;
    let tag = format!("v{version}");
    println!("📝 Generating changelog entry for {tag}…");
    spawn(
        "changelog",
        "git",
        &["cliff", "--tag", &tag, "--prepend", "CHANGELOG.md"],
    )?;
    println!("✅ CHANGELOG.md updated (existing entries preserved).");
    Ok(())
}

/// `cargo xtask release <version>` — prepare a release.
///
/// 1. Run CI (unless --skip-ci).
/// 2. Bump version in workspace Cargo.toml.
/// 3. Generate changelog entry via git-cliff --prepend.
/// 4. Create a `v<version>` git tag.
/// 5. Commit with `chore(release): prepare <version>`.
pub fn release(version_spec: &str, skip_ci: bool) -> Result<()> {
    let current_version = read_workspace_version()?;
    let new_version = resolve_version(version_spec, &current_version)?;
    let tag = format!("v{new_version}");

    println!("🚀 Preparing release {tag} (was v{current_version})");

    // Step 1: CI.
    if skip_ci {
        println!("⏩ Skipping CI (--skip-ci).");
    } else {
        println!("🔍 Running CI pipeline…");
        ci().context("CI")?;
    }

    // Step 2: Bump version.
    println!("📦 Bumping version to {new_version}…");
    write_workspace_version(&new_version)?;

    // Step 3: Changelog.
    println!("📝 Generating changelog…");
    spawn(
        "changelog",
        "git",
        &["cliff", "--tag", &tag, "--prepend", "CHANGELOG.md"],
    )?;

    // Step 4: Tag.
    println!("🏷️  Creating tag {tag}…");
    spawn("tag", "git", &["tag", &tag])?;

    // Step 5: Commit.
    println!("💾 Committing…");
    spawn("commit", "git", &["add", "-A"])?;
    spawn(
        "commit",
        "git",
        &[
            "commit",
            "-m",
            &format!("chore(release): prepare {new_version}"),
        ],
    )?;

    println!();
    println!("✅ Release {tag} prepared!");
    println!();
    println!("Next steps:");
    println!("  git push origin trunk --tags");
    Ok(())
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
