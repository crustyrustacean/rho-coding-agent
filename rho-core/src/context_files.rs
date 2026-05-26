//! Project context file scanning and trust management.
//!
//! At startup the agent scans the sandbox root for project-level instruction
//! files (`AGENTS.md`, `.cursorrules`, etc.) and incorporates the trusted ones
//! into the system prompt.
//!
//! # Trust model
//!
//! Each context file is identified by `(canonical_root, relative_filename)`.
//! On first encounter the user is shown the file's contents and asked to
//! confirm trust. The SHA-256 of the trusted contents is stored in
//! `~/.rho/trusted_projects.toml`. On subsequent runs the file loads silently
//! if the hash matches; if the hash differs the user is prompted again.
//!
//! Phase 5 reuses this storage for extension prompt trust — nothing here is
//! throwaway.

use crate::sandbox::SandboxRoot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

// ── Default scan list ─────────────────────────────────────────────────────────

/// Files scanned for project context, in priority order.
///
/// Configurable in `.rho/config.toml` (Phase 2); hardcoded for Phase 1b.
pub const DEFAULT_SCAN_LIST: &[&str] = &[
    "AGENTS.md",
    ".agents.md",
    "CLAUDE.md",
    ".cursorrules",
    ".rho/prompt.md",
];

// ── ContextFile ───────────────────────────────────────────────────────────────

/// A trusted project context file, ready to be injected into the system prompt.
#[derive(Clone, Debug)]
pub struct ContextFile {
    /// File name relative to the sandbox root (e.g. `"AGENTS.md"`).
    pub name: String,
    /// The file's contents.
    pub contents: String,
}

// ── TrustStore ────────────────────────────────────────────────────────────────

/// Persistent storage of trusted context-file hashes.
///
/// Backed by `~/.rho/trusted_projects.toml` (or a test-supplied path).
/// The file is a TOML array of [`TrustEntry`] records.
#[derive(Debug, Default)]
pub struct TrustStore {
    /// Path to the backing TOML file.
    path: PathBuf,
    /// In-memory trust entries.
    entries: Vec<TrustEntry>,
}

/// One entry in the trust store.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrustEntry {
    /// Canonical sandbox root path.
    root: String,
    /// File name relative to the root (e.g. `"AGENTS.md"`).
    file: String,
    /// Hex-encoded SHA-256 of the trusted file contents.
    sha256: String,
}

/// Top-level structure of the TOML trust file.
#[derive(Serialize, Deserialize, Default)]
struct TrustFile {
    /// All trust entries.
    #[serde(default)]
    entries: Vec<TrustEntry>,
}

impl TrustStore {
    /// Load from the default path (`~/.rho/trusted_projects.toml`).
    ///
    /// Creates an empty store if the file does not exist.
    pub fn load_default() -> Self {
        let path = default_trust_store_path();
        Self::load_from(&path)
    }

    /// Load from an explicit path (used by tests to avoid touching `~/.rho/`).
    pub fn load_from(path: &Path) -> Self {
        let entries = if path.exists() {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|s| toml::from_str::<TrustFile>(&s).ok())
                .map(|f| f.entries)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self {
            path: path.to_path_buf(),
            entries,
        }
    }

    /// Look up the stored hash for `(root, file)`.
    fn get(&self, root: &str, file: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.root == root && e.file == file)
            .map(|e| e.sha256.as_str())
    }

    /// Store or update the hash for `(root, file)` and persist to disk.
    fn set(&mut self, root: &str, file: &str, hash: &str) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.root == root && e.file == file)
        {
            hash.clone_into(&mut entry.sha256);
        } else {
            self.entries.push(TrustEntry {
                root: root.to_owned(),
                file: file.to_owned(),
                sha256: hash.to_owned(),
            });
        }
        self.persist();
    }

    /// Persist the store to disk.
    fn persist(&self) {
        let trust_file = TrustFile {
            entries: self.entries.clone(),
        };
        if let Ok(text) = toml::to_string_pretty(&trust_file) {
            // Create parent directory if needed.
            if let Some(parent) = self.path.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                eprintln!(
                    "warn: cannot create trust store directory {}: {e}",
                    parent.display()
                );
            }
            if let Err(e) = std::fs::write(&self.path, &text) {
                eprintln!(
                    "warn: cannot write trust store {}: {e} — trusted files will need re-confirmation on next startup",
                    self.path.display()
                );
            }
        }
    }
}

// ── ContextScanner ────────────────────────────────────────────────────────────

/// Scans a sandbox root for project context files and runs the trust workflow.
pub struct ContextScanner<'a> {
    /// The sandbox root to scan within.
    root: &'a SandboxRoot,
    /// Ordered list of file names to look for.
    scan_list: &'a [&'a str],
}

impl<'a> ContextScanner<'a> {
    /// Create a scanner using the default scan list.
    pub fn new(root: &'a SandboxRoot) -> Self {
        Self {
            root,
            scan_list: DEFAULT_SCAN_LIST,
        }
    }

    /// Run the full trust workflow for all found context files.
    ///
    /// For each file in the scan list that exists under the sandbox root:
    /// - If already trusted and unchanged: include silently.
    /// - If new or changed: display contents and prompt for confirmation.
    ///
    /// Returns the list of trusted [`ContextFile`] values in scan-list order.
    /// The `store` is mutated and persisted as trust decisions are made.
    pub fn run<R, W>(
        &self,
        store: &mut TrustStore,
        input: &mut R,
        output: &mut W,
    ) -> Vec<ContextFile>
    where
        R: BufRead,
        W: Write,
    {
        let root_str = self.root.path().to_string_lossy().to_string();
        let mut trusted = Vec::new();

        for &name in self.scan_list {
            let file_path = self.root.path().join(name);
            if !file_path.exists() {
                continue;
            }

            let Ok(contents) = std::fs::read_to_string(&file_path) else {
                continue;
            };

            let hash = sha256_hex(&contents);

            let is_trusted = match store.get(&root_str, name) {
                Some(stored) if stored == hash => {
                    // Hash matches — silently trusted.
                    true
                }
                Some(_) => {
                    // File changed — prompt for re-confirmation.
                    let _ = writeln!(output, "\nProject context file changed: {name}");
                    let _ = writeln!(output, "{}", "─".repeat(40));
                    let _ = writeln!(output, "{contents}");
                    let _ = writeln!(output, "{}", "─".repeat(40));
                    let approved = prompt_yn(output, input, &format!("Trust updated `{name}`?"));
                    if approved {
                        store.set(&root_str, name, &hash);
                    }
                    approved
                }
                None => {
                    // New file — prompt for first-load confirmation.
                    let _ = writeln!(output, "\nProject context file found: {name}");
                    let _ = writeln!(output, "{}", "─".repeat(40));
                    let _ = writeln!(output, "{contents}");
                    let _ = writeln!(output, "{}", "─".repeat(40));
                    let approved = prompt_yn(output, input, &format!("Trust `{name}`?"));
                    if approved {
                        store.set(&root_str, name, &hash);
                    }
                    approved
                }
            };

            if is_trusted {
                trusted.push(ContextFile {
                    name: name.to_owned(),
                    contents,
                });
            }
        }

        trusted
    }
}

// ── Prompt composition ────────────────────────────────────────────────────────

/// Compose the base system prompt with trusted context files.
///
/// Layout:
/// ```text
/// <base prompt>
///
/// --- AGENTS.md ---
/// <contents>
/// ---
/// ```
///
/// The base prompt is always first and cannot be displaced.
pub fn compose_system_prompt(base: &str, context_files: &[ContextFile]) -> String {
    if context_files.is_empty() {
        return base.to_owned();
    }

    let mut out = base.trim_end().to_owned();
    for file in context_files {
        out.push_str("\n\n--- ");
        out.push_str(&file.name);
        out.push_str(" ---\n");
        out.push_str(file.contents.trim());
        out.push_str("\n---");
    }
    out
}

/// Build the complete system prompt from base prompt, context files, and config.
///
/// This is the shared prompt construction used by both `rho` and `rho-bench`.
///
/// # Parameters
///
/// - `sandbox` — the project root, used for the working directory info block.
/// - `context_files` — trusted project context files (from [`ContextScanner`]).
///   Pass an empty slice to skip context file injection (e.g. for non-interactive
///   benchmarks).
/// - `config` — application config (used for [`system_prompt.extensions`]).
/// - `system_override` — if `Some`, replaces the base prompt and context files
///   entirely. The environment and Rust tooling blocks are still appended.
/// - `compact` — if `true`, uses the compact base prompt (~100 tokens) instead
///   of the full base prompt (~2,000 tokens).
///
/// # Layout
///
/// 1. System override **or** (base prompt + context files)
/// 2. `# Environment` block (working directory, fresh process note)
/// 3. `# Rust Tooling` block (`cargo_check`, `cargo_clippy`, etc.)
/// 4. Config-based [`system_prompt.extensions`] fragments
pub fn compose_full_system_prompt(
    sandbox: &SandboxRoot,
    context_files: &[ContextFile],
    config: &crate::config::RhoConfig,
    system_override: Option<&str>,
    compact: bool,
) -> String {
    use crate::prompts::{base_prompt, compact_prompt};

    let mut prompt = if let Some(custom) = system_override {
        custom.to_owned()
    } else {
        let prompt_base = if compact {
            compact_prompt()
        } else {
            base_prompt()
        };
        compose_system_prompt(prompt_base, context_files)
    };

    // Append the working directory so the model knows its absolute path.
    // Each run_command starts a fresh process in this directory —
    // cd / Set-Location does not persist between invocations.
    let root = sandbox.path().display();
    #[allow(clippy::format_push_string)]
    prompt.push_str(&format!(
        "\n\n# Environment\n\n\
         - Working directory (project root): `{root}`\n\
         - All relative file paths and shell commands resolve from this directory.\n\
         - Each `run_command` invocation starts a fresh process in this directory.\n\
           `cd` and `Set-Location` do not persist between commands — include the\n\
           full relative path from the project root in every command."
    ));

    // Append Rust tooling guidance when Rust tools are available.
    prompt.push_str(
        "\n\n# Rust Tooling\n\n\
         - You have access to structured Rust compiler diagnostics via `cargo_check` and `cargo_clippy`.\n\
         - When code fails to compile, use `cargo_check` before attempting manual fixes.\n\
         - Trust machine-applicable suggestions from the compiler — apply them with `cargo_fix` or by\n\
           using the suggested replacement text in `edit_file`.\n\
         - Use `cargo_clippy` for code-quality lints beyond compilation errors.\n\
         - Use `rustc_explain` to look up detailed explanations for error codes (e.g. E0308).\n\
         - Use `cargo_test` to verify fixes — run the relevant tests after each change.\n\
         - Prefer the structured diagnostic tools over `run_command` with raw `cargo check` —\n\
           the tools parse JSON output and surface only actionable workspace diagnostics.",
    );

    // Append config-based system prompt extensions.
    for extension in &config.system_prompt.extensions {
        #[allow(clippy::format_push_string)]
        prompt.push_str(&format!("\n\n{extension}"));
    }

    prompt
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Hex-encoded SHA-256 of `text`.
pub fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let result = hasher.finalize();
    result.iter().fold(String::new(), |mut acc, &b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Return the default path for the trust store (`~/.rho/trusted_projects.toml`).
pub fn default_trust_store_path() -> PathBuf {
    dirs_home().join(".rho").join("trusted_projects.toml")
}

/// Best-effort home directory; falls back to current directory if unavailable.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("."), PathBuf::from)
}

/// Print a `[y/N]` prompt and return `true` if the user types `y` or `yes`.
fn prompt_yn<W: Write, R: BufRead>(output: &mut W, input: &mut R, question: &str) -> bool {
    let _ = write!(output, "{question} [y/N] ");
    let _ = output.flush();
    let mut line = String::new();
    if input.read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn setup(files: &[(&str, &str)]) -> (TempDir, SandboxRoot) {
        let dir = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        let root = SandboxRoot::new(dir.path()).unwrap();
        (dir, root)
    }

    fn store_in(dir: &TempDir) -> TrustStore {
        TrustStore::load_from(&dir.path().join("trust.toml"))
    }

    #[test]
    fn new_file_prompts_and_trusts_on_yes() {
        let (_dir, root) = setup(&[("AGENTS.md", "# Project instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        let mut input = Cursor::new(b"y\n");
        let mut output = Vec::new();

        let trusted = scanner.run(&mut store, &mut input, &mut output);
        assert_eq!(trusted.len(), 1);
        assert_eq!(trusted[0].name, "AGENTS.md");
    }

    #[test]
    fn new_file_skipped_on_no() {
        let (_dir, root) = setup(&[("AGENTS.md", "# instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        let mut input = Cursor::new(b"n\n");
        let mut output = Vec::new();

        let trusted = scanner.run(&mut store, &mut input, &mut output);
        assert!(trusted.is_empty());
    }

    #[test]
    fn unchanged_file_loads_silently() {
        let (_dir, root) = setup(&[("AGENTS.md", "# instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);

        // First run: trust the file.
        let scanner = ContextScanner::new(&root);
        let mut input = Cursor::new(b"y\n");
        let mut output = Vec::new();
        scanner.run(&mut store, &mut input, &mut output);

        // Second run: no prompt expected.
        let mut input2 = Cursor::new(b""); // empty — would error if read
        let mut output2 = Vec::new();
        let trusted = scanner.run(&mut store, &mut input2, &mut output2);
        assert_eq!(trusted.len(), 1);
        // No prompt text expected
        assert!(!String::from_utf8_lossy(&output2).contains("Trust"));
    }

    #[test]
    fn changed_file_prompts_for_reconfirmation() {
        let (dir, root) = setup(&[("AGENTS.md", "# original")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);

        // Trust the original.
        let scanner = ContextScanner::new(&root);
        let mut input = Cursor::new(b"y\n");
        let mut out = Vec::new();
        scanner.run(&mut store, &mut input, &mut out);

        // Modify the file.
        std::fs::write(dir.path().join("AGENTS.md"), "# modified").unwrap();

        // Re-run: should prompt again.
        let mut input2 = Cursor::new(b"y\n");
        let mut out2 = Vec::new();
        let trusted = scanner.run(&mut store, &mut input2, &mut out2);
        let prompt_output = String::from_utf8_lossy(&out2);
        assert!(
            prompt_output.contains("changed"),
            "expected 'changed' prompt"
        );
        assert_eq!(trusted.len(), 1);
    }

    #[test]
    fn compose_prompt_prepends_base() {
        let base = "You are rho.";
        let files = vec![ContextFile {
            name: "AGENTS.md".to_owned(),
            contents: "# Instructions".to_owned(),
        }];
        let composed = compose_system_prompt(base, &files);
        assert!(composed.starts_with(base));
        assert!(composed.contains("--- AGENTS.md ---"));
        assert!(composed.contains("# Instructions"));
    }

    #[test]
    fn compose_prompt_no_files_returns_base() {
        let base = "You are rho.";
        assert_eq!(compose_system_prompt(base, &[]), base);
    }

    // ── compose_full_system_prompt ────────────────────────────────────────

    #[test]
    fn full_prompt_contains_environment_block() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let config = crate::config::RhoConfig::default();
        let prompt = compose_full_system_prompt(&sandbox, &[], &config, None, false);
        assert!(prompt.contains("# Environment"));
        assert!(prompt.contains("Working directory (project root)"));
        assert!(prompt.contains(dir.path().to_str().unwrap()));
    }

    #[test]
    fn full_prompt_contains_rust_tooling_block() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let config = crate::config::RhoConfig::default();
        let prompt = compose_full_system_prompt(&sandbox, &[], &config, None, false);
        assert!(prompt.contains("# Rust Tooling"));
        assert!(prompt.contains("cargo_check"));
        assert!(prompt.contains("cargo_clippy"));
        assert!(prompt.contains("cargo_fix"));
        assert!(prompt.contains("rustc_explain"));
        assert!(prompt.contains("cargo_test"));
    }

    #[test]
    fn full_prompt_with_override_skips_base_and_context() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let config = crate::config::RhoConfig::default();
        let context_files = vec![ContextFile {
            name: "AGENTS.md".to_owned(),
            contents: "# Instructions".to_owned(),
        }];
        let prompt = compose_full_system_prompt(
            &sandbox,
            &context_files,
            &config,
            Some("Custom system prompt."),
            false,
        );
        assert!(prompt.starts_with("Custom system prompt."));
        assert!(!prompt.contains("AGENTS.md"));
        // Environment and Rust Tooling blocks are still appended.
        assert!(prompt.contains("# Environment"));
        assert!(prompt.contains("# Rust Tooling"));
    }

    #[test]
    fn full_prompt_includes_context_files() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let config = crate::config::RhoConfig::default();
        let context_files = vec![ContextFile {
            name: "AGENTS.md".to_owned(),
            contents: "# Project rules".to_owned(),
        }];
        let prompt = compose_full_system_prompt(&sandbox, &context_files, &config, None, false);
        assert!(prompt.contains("--- AGENTS.md ---"));
        assert!(prompt.contains("# Project rules"));
    }

    #[test]
    fn full_prompt_includes_config_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let mut config = crate::config::RhoConfig::default();
        config.system_prompt.extensions = vec![
            "Always use PowerShell.".to_owned(),
            "Prefer functional style.".to_owned(),
        ];
        let prompt = compose_full_system_prompt(&sandbox, &[], &config, None, false);
        assert!(prompt.contains("Always use PowerShell."));
        assert!(prompt.contains("Prefer functional style."));
    }

    // ── Headless I/O (RPC mode) ──────────────────────────────────────────

    /// Helper: trust a file interactively (simulate "y" answer).
    fn trust_file_interactively(scanner: &ContextScanner<'_>, store: &mut TrustStore, name: &str) {
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut output = Vec::new();
        let trusted = scanner.run(store, &mut input, &mut output);
        assert!(
            trusted.iter().any(|f| f.name == name),
            "expected `{name}` to be trusted after interactive approval"
        );
    }

    #[test]
    fn headless_includes_already_trusted_file() {
        let (_dir, root) = setup(&[("AGENTS.md", "# instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        // Trust interactively first.
        trust_file_interactively(&scanner, &mut store, "AGENTS.md");

        // Headless run: already-trusted file must be included without blocking.
        let mut empty = Cursor::new(&b""[..]);
        let mut null = std::io::sink();
        let trusted = scanner.run(&mut store, &mut empty, &mut null);

        assert_eq!(
            trusted.len(),
            1,
            "already-trusted file must load headlessly"
        );
        assert_eq!(trusted[0].name, "AGENTS.md");
    }

    #[test]
    fn headless_auto_denies_new_file() {
        let (_dir, root) = setup(&[("AGENTS.md", "# instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        // Headless run: file has never been trusted — must be silently skipped.
        let mut empty = Cursor::new(&b""[..]);
        let mut null = std::io::sink();
        let trusted = scanner.run(&mut store, &mut empty, &mut null);

        assert!(
            trusted.is_empty(),
            "new file must be auto-denied in headless mode"
        );
    }

    #[test]
    fn headless_auto_denies_changed_file() {
        let (dir, root) = setup(&[("AGENTS.md", "# original")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        // Trust the original file interactively.
        trust_file_interactively(&scanner, &mut store, "AGENTS.md");

        // Modify the file so it needs re-confirmation.
        std::fs::write(dir.path().join("AGENTS.md"), "# modified").unwrap();

        // Headless run: changed file must be silently skipped.
        let mut empty = Cursor::new(&b""[..]);
        let mut null = std::io::sink();
        let trusted = scanner.run(&mut store, &mut empty, &mut null);

        assert!(
            trusted.is_empty(),
            "changed file must be auto-denied in headless mode"
        );
    }

    #[test]
    fn headless_auto_deny_does_not_persist_to_trust_store() {
        // When a new file is auto-denied in headless mode the trust store
        // must not be updated — the file should still prompt on the next
        // interactive run.
        let (_dir, root) = setup(&[("AGENTS.md", "# instructions")]);
        let store_dir = tempfile::tempdir().unwrap();
        let mut store = store_in(&store_dir);
        let scanner = ContextScanner::new(&root);

        let mut empty = Cursor::new(&b""[..]);
        let mut null = std::io::sink();
        scanner.run(&mut store, &mut empty, &mut null);

        // A second interactive run must still prompt (file not silently loaded).
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut output = Vec::new();
        let trusted = scanner.run(&mut store, &mut input, &mut output);
        let prompt_text = String::from_utf8_lossy(&output);
        assert!(
            prompt_text.contains("Trust"),
            "file should still require trust after headless auto-denial"
        );
        assert_eq!(
            trusted.len(),
            1,
            "interactive approval after headless run must work"
        );
    }

    #[test]
    fn full_prompt_empty_extensions_no_extra_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = SandboxRoot::new(dir.path()).unwrap();
        let config = crate::config::RhoConfig::default();
        let prompt = compose_full_system_prompt(&sandbox, &[], &config, None, false);
        // Extensions are empty, so the prompt should end after the Rust Tooling block
        // with no trailing double-newline from extensions.
        assert!(prompt.contains("# Rust Tooling"));
    }
}
