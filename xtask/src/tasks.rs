//! Individual task implementations.

use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};
use std::fmt::Write;

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

/// Parse a pricing field from an `OpenRouter` model JSON value.
/// `OpenRouter` returns pricing as strings (e.g. `"0.0000014"`) or sometimes as numbers.
fn parse_price_field(model: &serde_json::Value, key: &str) -> Option<f64> {
    let val = model.get("pricing")?.get(key)?;
    if let Some(s) = val.as_str() {
        return s.parse::<f64>().ok();
    }
    val.as_f64()
}

/// `cargo xtask generate-models` — fetch `OpenRouter` models and generate `catalog_generated.rs`.
#[allow(clippy::too_many_lines)]
pub fn generate_models() -> Result<()> {
    let root = workspace_root();

    // Paths.
    let overrides_path = format!("{root}/rho-ai/model-overrides.json");
    let output_path = format!("{root}/rho-ai/src/catalog_generated.rs");

    // 1. Fetch models from OpenRouter.
    println!("Fetching models from OpenRouter API...");
    let url = "https://openrouter.ai/api/v1/models";
    let response: serde_json::Value = reqwest::blocking::Client::new()
        .get(url)
        .send()
        .with_context(|| format!("failed to fetch {url}"))?
        .json()
        .with_context(|| format!("failed to parse {url} response as JSON"))?;

    let models = response
        .get("data")
        .and_then(|d| d.as_array())
        .context("expected `data` array in OpenRouter response")?;

    println!("  Found {} models from OpenRouter", models.len());

    // 2. Load overrides.
    println!("Loading overrides from {overrides_path}...");
    let overrides_str = std::fs::read_to_string(&overrides_path)
        .with_context(|| format!("failed to read {overrides_path}"))?;
    let overrides: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&overrides_str).context("failed to parse model-overrides.json")?;

    // 3. Generate Rust source.
    println!("Generating {output_path}...");
    let mut buf = String::new();
    buf.push_str(
        r"//! Auto-generated built-in model catalog.
//!
//! Generated by `cargo xtask generate-models` from OpenRouter's API.
//!
//! DO NOT EDIT -- regenerate with `cargo xtask generate-models`.
//!
//! Source: <https://openrouter.ai/api/v1/models>
//! Overrides: `rho-ai/model-overrides.json`

// Generated code: suppress pedantic lints that do not add value here.
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::unreadable_literal,
    clippy::excessive_precision,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
)]

",
    );
    buf.push_str("use super::catalog::{Model, ModelCost, ModelInput, ModelThinking};\n\n");
    buf.push_str("/// All built-in models from `OpenRouter` with manual overrides applied.\n");
    buf.push_str("pub fn built_in_models() -> Vec<Model> {\n");
    buf.push_str("    vec![\n");

    for m in models {
        let id = m
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        // Skip free-tier models (`:free` suffix) and routing aliases (`~` prefix).
        if id.starts_with('~') || id.ends_with(":free") {
            continue;
        }

        let name = m
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let context_length = m
            .get("context_length")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);

        // Provider is the first segment of the ID (e.g. "anthropic" from "anthropic/claude-sonnet-4").
        let provider = id.split('/').next().unwrap_or("");

        // Max completion tokens.
        let max_tokens = m
            .get("top_provider")
            .and_then(|tp| tp.get("max_completion_tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(context_length / 4); // fallback heuristic

        // Input modalities.
        let modalities = m
            .get("architecture")
            .and_then(|a| a.get("input_modalities"))
            .and_then(|v| v.as_array());
        let text = modalities.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("text")));
        let image = modalities.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("image")));

        // Pricing (per-token, convert to per-million).
        // OpenRouter returns pricing as strings (e.g. "0.0000014") or sometimes numbers.
        let prompt_per_token = parse_price_field(m, "prompt").unwrap_or(0.0);
        let completion_per_token = parse_price_field(m, "completion").unwrap_or(0.0);
        let cache_read_per_token = parse_price_field(m, "input_cache_read");
        let cache_write_per_token = parse_price_field(m, "input_cache_write");

        // Convert from per-token to per-million-tokens.
        let cost_input = prompt_per_token * 1_000_000.0;
        let cost_output = completion_per_token * 1_000_000.0;
        let cost_cache_read = cache_read_per_token.map_or(0.0, |v| v * 1_000_000.0);
        let cost_cache_write = cache_write_per_token.map_or(0.0, |v| v * 1_000_000.0);

        // Thinking overrides.
        let mut thinking_supported = false;
        let mut thinking_format: Option<String> = None;
        // Optional caps on the advertised context window / max completion
        // tokens. OpenRouter advertises beta/extended values (e.g. 1M for
        // claude-sonnet-4) that aren't honored by all routes (BYOK to
        // Vertex/Bedrock caps at the model's standard window). These caps let
        // the catalog reflect what rho can actually use.
        let mut context_window_override: Option<u64> = None;
        let mut max_tokens_override: Option<u64> = None;
        if let Some(override_data) = overrides.get(id) {
            thinking_supported = override_data
                .get("thinking")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            thinking_format = override_data
                .get("thinking_format")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            context_window_override = override_data
                .get("context_window")
                .and_then(serde_json::Value::as_u64);
            max_tokens_override = override_data
                .get("max_tokens")
                .and_then(serde_json::Value::as_u64);
        }

        // Apply caps: never exceed the advertised value, but allow lowering it.
        let context_length = context_window_override
            .map_or(context_length, |cap| cap.min(context_length));
        let max_tokens = max_tokens_override
            .map_or(max_tokens, |cap| cap.min(max_tokens));

        // Format thinking_format for output.
        let thinking_format_str = match &thinking_format {
            Some(f) => format!("Some({f:?}.to_string())"),
            None => "None".to_string(),
        };

        // Emit the Model literal.
        buf.push_str("        Model {\n");
        writeln!(buf, "            id: {id:?}.to_string(),").unwrap();
        writeln!(buf, "            name: {name:?}.to_string(),").unwrap();
        writeln!(buf, "            provider: {provider:?}.to_string(),").unwrap();
        writeln!(buf, "            context_window: {context_length},").unwrap();
        writeln!(buf, "            max_tokens: {max_tokens},").unwrap();
        writeln!(
            buf,
            "            input: ModelInput {{ text: {text}, image: {image} }},"
        )
        .unwrap();
        buf.push_str("            cost: ModelCost {\n");
        writeln!(buf, "                input: {cost_input:.10},").unwrap();
        writeln!(buf, "                output: {cost_output:.10},").unwrap();
        writeln!(buf, "                cache_read: {cost_cache_read:.10},").unwrap();
        writeln!(buf, "                cache_write: {cost_cache_write:.10},").unwrap();
        buf.push_str("            },\n");
        buf.push_str("            thinking: ModelThinking {\n");
        writeln!(buf, "                supported: {thinking_supported},").unwrap();
        writeln!(buf, "                format: {thinking_format_str},").unwrap();
        buf.push_str("            },\n");
        buf.push_str("        },\n");
    }

    buf.push_str("    ]\n}");

    // 4. Write output.
    std::fs::write(&output_path, &buf).with_context(|| format!("failed to write {output_path}"))?;

    let model_count = buf.matches("Model {").count();
    println!("Generated {model_count} models -> {output_path}");
    Ok(())
}

/// `cargo xtask schema` -- generate/update the `OpenRPC` 1.3.1 schema.
///
/// Reads the version from workspace `Cargo.toml`, injects it into
/// `docs/rpc-schema/openrpc.json`, and writes the result.
pub fn schema() -> Result<()> {
    let root = workspace_root();
    let version = read_workspace_version()?;

    let schema_path = std::path::PathBuf::from(root).join("docs/rpc-schema/openrpc.json");
    let content =
        std::fs::read_to_string(&schema_path).context("schema: failed to read openrpc.json")?;
    let mut spec: serde_json::Value =
        serde_json::from_str(&content).context("schema: failed to parse openrpc.json")?;

    spec["info"]["version"] = serde_json::json!(version);

    let output = serde_json::to_string_pretty(&spec)?;
    std::fs::write(&schema_path, output.as_bytes())
        .context("schema: failed to write openrpc.json")?;

    println!("📝 docs/rpc-schema/openrpc.json updated (version: {version})");
    Ok(())
}
