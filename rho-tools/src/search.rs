//! Read-tier tools: structured content search and file finding.
//!
//! Phase 0 of the engine-upgrades plan (rho-brain `01a02263`): give the model
//! sandbox-confined, gitignore-aware alternatives to shell one-liners
//! (`Select-String`, `grep`, `Get-ChildItem -Recurse`) so that locating code
//! and files never needs `run_command`.
//!
//! - [`SearchFiles`] — regex content search over the tree, returning capped
//!   `path:line: text` matches. Built on the ripgrep engine crates
//!   (`grep-regex` + `grep-searcher`) — the same machinery `rg` runs on.
//! - [`FindFiles`] — name-glob file finder over the `ignore` crate's walker
//!   (already a dependency for `list_dir`).
//!
//! Both are `ToolRisk::Read`, confined to the sandbox root, and cap their
//! output so a broad pattern cannot flood the context window.

use crate::error::ToolError;
use async_trait::async_trait;
use globset::GlobBuilder;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use rho_core::tool::{CancellationToken, Tool, ToolOutcome, ToolResult, ToolRisk};
use rho_core::{Result, SandboxRoot, ToolName};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tracing::warn;

/// Hard ceiling on `max_results` for `search_files`, regardless of what the
/// model asks for. Keeps the worst case bounded even for pathologically
/// broad patterns (`.*` over a large tree).
const SEARCH_MAX_RESULTS_CEILING: usize = 500;

/// Default match cap for `search_files`.
const SEARCH_MAX_RESULTS_DEFAULT: usize = 100;

/// Hard ceiling on `max_results` for `find_files`.
const FIND_MAX_RESULTS_CEILING: usize = 1000;

/// Default result cap for `find_files`.
const FIND_MAX_RESULTS_DEFAULT: usize = 200;

/// Maximum characters of a matched line to include in output. Minified or
/// bundled files can contain megabyte "lines"; a search result must not.
const MAX_LINE_DISPLAY_CHARS: usize = 240;

// ── search_files ─────────────────────────────────────────────────────────────

/// Regex content search across files in the sandbox.
///
/// Returns matches as `path:line: text` lines, capped at `max_results`
/// (default 100, ceiling 500). Respects `.gitignore` rules and skips hidden
/// files, matching what the model would see from `rg`/`git grep` — not the
/// raw filesystem.
pub struct SearchFiles {
    /// Sandbox root; every searched path must resolve within it.
    root: SandboxRoot,
}

impl SearchFiles {
    /// Create the tool bound to a sandbox root.
    #[must_use]
    pub fn new(root: SandboxRoot) -> Self {
        Self { root }
    }

    /// Run the search: regex over a gitignore-aware walk of `scope`.
    ///
    /// Returns the collected `path:line: text` lines (possibly including a
    /// trailing truncation notice). Errors are tool-level and land in the
    /// returned lines as a single-element vector only for cancellation; all
    /// other failures surface via [`ToolError`].
    fn search(
        pattern: &str,
        scope: &Path,
        glob: Option<&str>,
        ignore_case: bool,
        max_results: usize,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<String>, ToolError> {
        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(ignore_case)
            .build(pattern)
        {
            Ok(m) => m,
            Err(e) => {
                return Err(ToolError::Internal {
                    message: format!(
                        "invalid regex pattern '{pattern}': {e}. Note: the regex engine \
                         does not support look-around assertions (lookahead/lookbehind); \
                         rewrite without them, e.g. using character classes or anchors."
                    ),
                });
            }
        };

        let mut walker_builder = ignore::WalkBuilder::new(scope);
        walker_builder
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .hidden(true)
            .require_git(false);
        if let Some(g) = glob {
            let compiled = GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| ToolError::Internal {
                    message: format!("invalid glob '{g}': {e}"),
                })?
                .compile_matcher();
            walker_builder.filter_entry(move |entry| {
                // Let directories through so the walk can descend; filter
                // only files.
                entry.file_type().is_none_or(|ft| ft.is_dir()) || compiled.is_match(entry.path())
            });
        }

        let count = AtomicUsize::new(0);
        let saw_overflow = AtomicBool::new(false);
        let mut lines: Vec<String> = Vec::new();

        for entry in walker_builder.build() {
            if cancel.is_cancelled() {
                return Ok(vec!["cancelled".to_string()]);
            }
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!(error = %err, "search_files: walk error, skipping entry");
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            if count.load(Ordering::Relaxed) >= max_results {
                break;
            }
            let path = entry.path();
            let display = path
                .strip_prefix(scope)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let mut sink = CollectingSink {
                path_display: display,
                lines: &mut lines,
                count: &count,
                cap: max_results,
                saw_overflow: &saw_overflow,
            };
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(0))
                .line_number(true)
                .build();
            if let Err(err) = searcher.search_path(&matcher, path, &mut sink) {
                warn!(path = %path.display(), error = %err, "search_files: file search error");
            }
        }

        if saw_overflow.load(Ordering::Relaxed) {
            lines.push(format!(
                "… more matches truncated (showing first {max_results}; raise max_results up to \
                 {SEARCH_MAX_RESULTS_CEILING} or narrow the pattern/path)"
            ));
        }
        Ok(lines)
    }
}

/// Sink collecting matches for one file into `path:line: text` lines.
///
/// The counters are shared across files so the cap is global, and
/// `saw_overflow` records whether any match landed *after* the cap (the
/// truncation notice must not fire without evidence).
struct CollectingSink<'a> {
    /// Display prefix (path relative to the search scope) for each line.
    path_display: String,
    /// Collected `path:line: text` lines.
    lines: &'a mut Vec<String>,
    /// Global match counter shared across files.
    count: &'a AtomicUsize,
    /// Hard cap on matches.
    cap: usize,
    /// Whether a match was observed after the cap was reached.
    saw_overflow: &'a AtomicBool,
}

impl Sink for CollectingSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.cap {
            self.saw_overflow.store(true, Ordering::Relaxed);
            return Ok(false);
        }
        let line = String::from_utf8_lossy(mat.bytes());
        let trimmed = line.trim_end();
        // Cap individual line length so a minified file cannot inject a
        // multi-megabyte "line" into the results.
        let display: String = if trimmed.chars().count() > MAX_LINE_DISPLAY_CHARS {
            let cut: String = trimmed.chars().take(MAX_LINE_DISPLAY_CHARS).collect();
            format!("{cut}…")
        } else {
            trimmed.to_owned()
        };
        self.lines.push(format!(
            "{}:{}: {}",
            self.path_display,
            mat.line_number().unwrap_or(0),
            display
        ));
        Ok(true)
    }
}

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> ToolName {
        ToolName::from("search_files")
    }

    fn description(&self) -> &str {
        "Search file contents with a regex across the project (or a subdirectory). \
         Returns matches as 'path:line: text', capped at max_results. \
         Respects .gitignore and skips hidden files and binary files. \
         PREFER this over run_command with grep/Select-String for finding where \
         code, symbols, strings, or TODOs live — it is sandbox-safe, faster, \
         and returns structured results. \
         Regex notes: no look-around (lookahead/lookbehind) support; set \
         ignore_case=true instead of using (?i)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for (no look-around)."
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory to scope the search to (relative to project root). Defaults to the project root."
                },
                "glob": {
                    "type": "string",
                    "description": "Optional glob filter for files to search, e.g. '*.rs'."
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case-insensitive matching. Defaults to false."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum matches to return (default 100, max 500)."
                }
            },
            "required": ["pattern"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let pattern = arguments["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::MissingArgument {
                name: "pattern".to_string(),
            })?
            .to_owned();
        let scope_arg = arguments["path"].as_str().unwrap_or(".").to_owned();
        let glob = arguments["glob"].as_str().map(str::to_owned);
        let ignore_case = arguments["ignore_case"].as_bool().unwrap_or(false);
        let max_results =
            arguments["max_results"]
                .as_u64()
                .map_or(SEARCH_MAX_RESULTS_DEFAULT, |n| {
                    usize::try_from(n.clamp(
                        1,
                        u64::try_from(SEARCH_MAX_RESULTS_CEILING).unwrap_or(u64::MAX),
                    ))
                    .unwrap_or(SEARCH_MAX_RESULTS_DEFAULT)
                });

        // Sandbox validation: the scope must resolve inside the root.
        let candidate = self.root.path().join(&scope_arg);
        let safe_scope = match self.root.validate(&candidate) {
            Ok(p) => p,
            Err(e) => {
                warn!(path = %scope_arg, error = %e, "search_files: sandbox validation failed");
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "search_files: path '{scope_arg}' is outside the project sandbox"
                ))));
            }
        };

        let lines = match Self::search(
            &pattern,
            &safe_scope,
            glob.as_deref(),
            ignore_case,
            max_results,
            &cancel,
        ) {
            Ok(lines) => lines,
            Err(ToolError::Internal { message }) => {
                return Ok(ToolOutcome::Immediate(ToolResult::error(message)));
            }
            Err(e) => return Err(e.into()),
        };

        let output = if lines.is_empty() {
            format!("No matches for '{pattern}'")
        } else {
            lines.join("\n")
        };
        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

// ── find_files ───────────────────────────────────────────────────────────────

/// Find files by name glob across the sandbox.
///
/// Returns matching paths (relative to the search scope), capped at
/// `max_results` (default 200, ceiling 1000). Gitignore-aware; hidden files
/// and directories are skipped. Directories themselves are never returned —
/// use `list_dir` for directory listings.
pub struct FindFiles {
    /// Sandbox root; every candidate path must resolve within it.
    root: SandboxRoot,
}

impl FindFiles {
    /// Create the tool bound to a sandbox root.
    #[must_use]
    pub fn new(root: SandboxRoot) -> Self {
        Self { root }
    }
}

#[async_trait]
impl Tool for FindFiles {
    fn name(&self) -> ToolName {
        ToolName::from("find_files")
    }

    fn description(&self) -> &str {
        "Find files by name pattern (glob) under the project (or a subdirectory). \
         Returns matching file paths, capped at max_results. \
         Respects .gitignore and skips hidden files. Directories are not returned. \
         PREFER this over run_command with Get-ChildItem -Recurse/find/ls — \
         it is sandbox-safe and returns a clean path list. \
         The pattern matches against the file NAME only (e.g. '*.rs', '*test*', \
         'Cargo.toml'), not the full path."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob matched against file names, e.g. '*.rs', '*test*', 'Cargo.toml'."
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory to scope the search to (relative to project root). Defaults to the project root."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum paths to return (default 200, max 1000)."
                }
            },
            "required": ["pattern"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let pattern = arguments["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::MissingArgument {
                name: "pattern".to_string(),
            })?
            .to_owned();
        let scope_arg = arguments["path"].as_str().unwrap_or(".").to_owned();
        let max_results = arguments["max_results"]
            .as_u64()
            .map_or(FIND_MAX_RESULTS_DEFAULT, |n| {
                usize::try_from(n.clamp(
                    1,
                    u64::try_from(FIND_MAX_RESULTS_CEILING).unwrap_or(u64::MAX),
                ))
                .unwrap_or(FIND_MAX_RESULTS_DEFAULT)
            });

        let matcher = match GlobBuilder::new(&pattern).literal_separator(false).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => {
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "invalid glob '{pattern}': {e}"
                ))));
            }
        };

        let candidate = self.root.path().join(&scope_arg);
        let safe_scope = match self.root.validate(&candidate) {
            Ok(p) => p,
            Err(e) => {
                warn!(path = %scope_arg, error = %e, "find_files: sandbox validation failed");
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "find_files: path '{scope_arg}' is outside the project sandbox"
                ))));
            }
        };

        let mut walker_builder = ignore::WalkBuilder::new(&*safe_scope);
        walker_builder
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .hidden(true)
            .require_git(false);

        let mut paths: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in walker_builder.build() {
            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!(error = %err, "find_files: walk error, skipping entry");
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let path = entry.path();
            if !matcher.is_match(path.file_name().unwrap_or_default()) {
                continue;
            }
            if paths.len() >= max_results {
                truncated = true;
                break;
            }
            let display = path
                .strip_prefix(&*safe_scope)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            paths.push(display);
        }

        let mut output = if paths.is_empty() {
            format!("No files matching '{pattern}'")
        } else {
            paths.join("\n")
        };
        if truncated {
            use std::fmt::Write as _;
            let _ = write!(
                output,
                "\n… more results truncated (showing first {max_results}; raise max_results up to {FIND_MAX_RESULTS_CEILING} or narrow the pattern/path)"
            );
        }
        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}
