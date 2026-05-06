//! File operation tools: [`ReadFile`], [`WriteFile`], [`ListDir`], and [`EditFile`].
//!
//! Phase 1b: sandbox enforcement via [`SandboxRoot`], and `<context>` framing on
//! `ReadFile` output so the model treats file contents as data, not instructions.
//!
//! Phase 2: [`ListDir`] with `.gitignore`-aware directory walking via the
//! `ignore` crate, and [`EditFile`] with exact-match replacement and validation.

use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};

// ── ReadFile ──────────────────────────────────────────────────────────────────

/// Read a file's text content.
///
/// Output is wrapped in `<context>...</context>` tags so the model treats it
/// as data rather than as instructions (defense-in-depth against prompt
/// injection via file contents).
///
/// Paths are validated against the [`SandboxRoot`] before any I/O.
pub struct ReadFile {
    /// Sandbox root — all reads are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> ToolName {
        ToolName::from("read_file")
    }

    fn description(&self) -> &str {
        "Read the text contents of a file within the project. \
         Returns the file's content wrapped in <context> tags."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to the project root)."
                }
            },
            "required": ["path"]
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
        let path_str = arguments["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("read_file: missing required argument `path`"))?;

        // Validate path is within the sandbox root.
        let safe_path = self.root.validate(path_str)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = match tokio::fs::read_to_string(&*safe_path).await {
            Ok(c) => c,
            Err(e) => {
                // Return as a tool error so the model can self-correct
                // (e.g. try a different path) instead of killing the loop.
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "read_file: failed to read `{path_str}`: {e}"
                ))));
            }
        };

        // Wrap in <context> framing — signals to the model that this is data,
        // not instructions. The system prompt reinforces this contract.
        let framed = format!("<context>\n{content}\n</context>");

        Ok(ToolOutcome::Immediate(ToolResult::success(framed)))
    }
}

// ── WriteFile ─────────────────────────────────────────────────────────────────

/// Write content to a file, creating it if it does not exist.
///
/// Paths are validated against the [`SandboxRoot`] before any I/O. Uses the
/// not-yet-existing-path validation so new files in the sandbox are allowed.
pub struct WriteFile {
    /// Sandbox root — all writes are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> ToolName {
        ToolName::from("write_file")
    }

    fn description(&self) -> &str {
        "Write text content to a file within the project. \
         Creates the file and any necessary parent directories if they do not exist; \
         overwrites it if it does."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to the project root)."
                },
                "content": {
                    "type": "string",
                    "description": "The text content to write."
                }
            },
            "required": ["path", "content"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let path_str = arguments["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write_file: missing required argument `path`"))?;
        let content = arguments["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write_file: missing required argument `content`"))?;

        // Use the write-variant validator that handles not-yet-existing paths.
        let safe_path = self.root.validate_for_write(path_str)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // Create parent directories if needed.
        if let Some(parent) = safe_path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "write_file: failed to create directories for `{path_str}`: {e}"
            ))));
        }

        if let Err(e) = tokio::fs::write(&*safe_path, content).await {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "write_file: failed to write `{path_str}`: {e}"
            ))));
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(format!(
            "wrote {} bytes to {path_str}",
            content.len()
        ))))
    }
}

// ── ListDir ───────────────────────────────────────────────────────────────────

/// List directory contents with `.gitignore` awareness.
///
/// Uses the `ignore` crate (from ripgrep) for walking, which respects
/// `.gitignore`, `.ignore`, and nested override files. By default, only
/// the top-level directory is listed; set `recursive: true` to walk subdirectories.
///
/// Paths are validated against the [`SandboxRoot`] before any I/O.
pub struct ListDir {
    /// Sandbox root — all paths are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> ToolName {
        ToolName::from("list_dir")
    }

    fn description(&self) -> &str {
        "List files and directories within the project. \
         Respects .gitignore rules by default. \
         Set recursive to true to walk subdirectories."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list (relative to the project root). Defaults to the project root."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list files recursively. Defaults to false."
                }
            },
            "required": []
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
        let path_str = arguments["path"].as_str().unwrap_or(".");
        let recursive = arguments["recursive"].as_bool().unwrap_or(false);

        // Validate path is within the sandbox root.
        // If the path doesn't exist, validate will error.
        // If path is "." we validate the root itself.
        let safe_path = if path_str == "." {
            self.root.path().to_path_buf()
        } else {
            // Resolve relative paths against the sandbox root before validation.
            let candidate = self.root.path().join(path_str);
            self.root.validate(&candidate)?.to_path_buf()
        };

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        if !safe_path.is_dir() {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "list_dir: `{path_str}` is not a directory"
            ))));
        }

        // Build the walker with .gitignore awareness.
        let mut builder = ignore::WalkBuilder::new(&safe_path);
        builder
            .hidden(false) // show hidden files (dotfiles like .agents.md)
            .git_ignore(true) // respect .gitignore
            .git_global(true) // respect global gitignore
            .git_exclude(true) // respect .git/info/exclude
            .ignore(true) // respect .ignore
            .require_git(false) // work even without a git repo
            .sort_by_file_name(std::cmp::Ord::cmp); // deterministic ordering

        if !recursive {
            builder.max_depth(Some(1));
        }

        let walker = builder.build();

        let mut entries = Vec::new();
        let mut error_count = 0u32;

        for entry in walker {
            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }

            match entry {
                Ok(e) => {
                    // Skip the root directory itself.
                    if e.path() == safe_path {
                        continue;
                    }

                    let relative = e.path().strip_prefix(&safe_path).unwrap_or(e.path());

                    let path_display = relative.to_string_lossy();

                    if e.file_type().is_some_and(|ft| ft.is_dir()) {
                        entries.push(format!("{path_display}/"));
                    } else {
                        entries.push(path_display.into_owned());
                    }
                }
                Err(_) => {
                    error_count += 1;
                }
            }
        }

        let mut output = entries.join("\n");
        if error_count > 0 {
            use std::fmt::Write;
            let _ = write!(output, "\n\n({error_count} entries could not be read)");
        }

        if output.is_empty() {
            output.clear();
            output.push_str("(empty directory)");
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

// ── EditFile ──────────────────────────────────────────────────────────────────

/// A single replacement operation for [`EditFile`].
#[derive(Clone, Debug)]
struct Edit {
    /// The exact text to find.
    old_text: String,
    /// The replacement text.
    new_text: String,
}

/// Apply targeted exact-match replacements to a file.
///
/// Validates that each `old_text` occurs exactly once in the file (not ambiguous),
/// that edits don't overlap, and applies them all in a single write.
///
/// Tree-sitter node-splitting validation is deferred to Phase 3.
pub struct EditFile {
    /// Sandbox root — all writes are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> ToolName {
        ToolName::from("edit_file")
    }

    fn description(&self) -> &str {
        "Apply targeted exact-match replacements to a file within the project. \
         Each edit specifies old_text to find and new_text to replace it with. \
         old_text must be an exact character-for-character copy of the text in the file, \
         including whitespace and newlines — it is NOT a regex or pattern. \
         All old_text occurrences must be unique (exactly one match each) and \
         edits must not overlap. The file is not modified if any edit fails validation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative to the project root)."
                },
                "edits": {
                    "type": "array",
                    "description": "List of replacements to apply.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {
                                "type": "string",
                                "description": "The exact literal text to find in the file. \
                                    Must match character-for-character including whitespace \
                                    and newlines. Not a regex — do not use \\s, .*, or \
                                    escape sequences for whitespace."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "The text to replace old_text with."
                            }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let path_str = arguments["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit_file: missing required argument `path`"))?;
        let edits_arg = arguments["edits"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("edit_file: missing required argument `edits`"))?;

        if edits_arg.is_empty() {
            return Ok(ToolOutcome::Immediate(ToolResult::error(
                "edit_file: no edits provided",
            )));
        }

        // Parse edits from JSON.
        let mut edits = Vec::with_capacity(edits_arg.len());
        for (i, edit_val) in edits_arg.iter().enumerate() {
            let old_text = edit_val["old_text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("edit_file: edit {i} missing `old_text`"))?
                .to_owned();
            let new_text = edit_val["new_text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("edit_file: edit {i} missing `new_text`"))?
                .to_owned();
            edits.push(Edit { old_text, new_text });
        }

        // Validate path is within the sandbox root.
        let safe_path = self.root.validate(path_str)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // Read the current file content.
        let content = match tokio::fs::read_to_string(&*safe_path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "edit_file: failed to read `{path_str}`: {e}"
                ))));
            }
        };

        // Validate all edits: each old_text must occur exactly once, and edits
        // must not overlap.
        let mut match_ranges: Vec<(usize, usize, &Edit)> = Vec::with_capacity(edits.len());

        for edit in &edits {
            let occurrences: Vec<_> = content.match_indices(&edit.old_text).collect();
            match occurrences.len() {
                0 => {
                    let hint = detect_regex_patterns(&edit.old_text).map_or_else(
                        String::new,
                        |patterns| {
                            format!(
                                " Hint: old_text contains regex-like patterns ({patterns}). \
                                 old_text must be an exact character-for-character match \
                                 of the file content, not a regex. Use read_file to see \
                                 the exact content, then copy the literal text."
                            )
                        },
                    );
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                        "edit_file: old_text not found in `{path_str}`: {:?}{hint}",
                        truncate_for_error(&edit.old_text, 80)
                    ))));
                }
                1 => {
                    let (start, _) = occurrences[0];
                    let end = start + edit.old_text.len();
                    match_ranges.push((start, end, edit));
                }
                _ => {
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                        "edit_file: old_text is ambiguous ({} matches) in `{path_str}`: {:?}",
                        occurrences.len(),
                        truncate_for_error(&edit.old_text, 80)
                    ))));
                }
            }
        }

        // Sort by start position to check for overlaps.
        match_ranges.sort_by_key(|(start, _, _)| *start);

        for window in match_ranges.windows(2) {
            let (_, end_a, _) = window[0];
            let (start_b, _, _) = window[1];
            if start_b < end_a {
                return Ok(ToolOutcome::Immediate(ToolResult::error(
                    "edit_file: edits overlap — two edits target the same region of the file",
                )));
            }
        }

        // Apply edits from last to first so earlier positions remain valid.
        let mut modified = content;
        for (start, end, edit) in match_ranges.into_iter().rev() {
            modified.replace_range(start..end, &edit.new_text);
        }

        // Write the modified content.
        if let Err(e) = tokio::fs::write(&*safe_path, &modified).await {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "edit_file: failed to write `{path_str}`: {e}"
            ))));
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(format!(
            "applied {} edit(s) to {path_str}",
            edits.len()
        ))))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Detect common regex metacharacter patterns in `old_text`.
///
/// Returns a comma-separated list of the regex-like tokens found, or `None`
/// if the text looks like a plausible literal string. This is a best-effort
/// heuristic — false positives are acceptable because the result is only
/// used in a diagnostic hint, not to block the operation.
fn detect_regex_patterns(text: &str) -> Option<String> {
    /// Patterns that virtually never appear in literal source code but are
    /// common when a model mistakenly treats `old_text` as a regex.
    const REGEX_TOKENS: &[&str] = &[
        r"\s*", r"\s+", r"\d+", r"\d*", r"\w+", r"\w*", r"\n", r"\t", r"\r", r".+", r".*",
    ];

    let mut found: Vec<&str> = Vec::new();
    for token in REGEX_TOKENS {
        if text.contains(token) && !found.contains(token) {
            found.push(token);
        }
    }

    if found.is_empty() {
        None
    } else {
        Some(found.join(", "))
    }
}

/// Truncate `text` to `max_len` characters for inclusion in error messages.
///
/// Appends "…" if truncation occurred.
fn truncate_for_error(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_owned()
    } else {
        let truncated: String = text.chars().take(max_len).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_for_error ────────────────────────────────────────────────

    #[test]
    fn truncate_short_text_unchanged() {
        assert_eq!(truncate_for_error("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_text() {
        let long = "a".repeat(100);
        let result = truncate_for_error(&long, 10);
        assert_eq!(result, "aaaaaaaaaa…");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        assert_eq!(truncate_for_error("hello", 5), "hello");
    }

    #[test]
    fn truncate_empty() {
        assert_eq!(truncate_for_error("", 10), "");
    }

    // ── detect_regex_patterns ─────────────────────────────────────────────

    #[test]
    fn detect_regex_finds_backslash_s_star() {
        let text = r"fn foo() {\s*bar()}";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\s*");
        assert!(result.unwrap().contains(r"\s*"));
    }

    #[test]
    fn detect_regex_finds_backslash_n() {
        let text = r"line1\nline2";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\n");
        assert!(result.unwrap().contains(r"\n"));
    }

    #[test]
    fn detect_regex_finds_multiple_patterns() {
        let text = r"fn foo() {\s*bar()\n}";
        let result = detect_regex_patterns(text);
        assert!(result.is_some());
        let found = result.unwrap();
        assert!(found.contains(r"\s*"), "should detect \\s*");
        assert!(found.contains(r"\n"), "should detect \\n");
    }

    #[test]
    fn detect_regex_clean_literal_text() {
        let text = "fn main() {\n    println!(\"hello\");\n}";
        let result = detect_regex_patterns(text);
        assert!(
            result.is_none(),
            "literal newlines should not trigger detection"
        );
    }

    #[test]
    fn detect_regex_empty_text() {
        assert!(detect_regex_patterns("").is_none());
    }

    #[test]
    fn detect_regex_dot_star() {
        let text = r"fn .*()";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect .*");
    }

    #[test]
    fn detect_regex_dot_plus() {
        let text = r"name: .+";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect .+");
        assert!(result.unwrap().contains(r".+"));
    }

    #[test]
    fn detect_regex_backslash_d() {
        let text = r"id: \d+";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\d+");
        assert!(result.unwrap().contains(r"\d+"));
    }

    #[test]
    fn detect_regex_backslash_d_star() {
        let text = r"count\d*";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\d*");
        assert!(result.unwrap().contains(r"\d*"));
    }

    #[test]
    fn detect_regex_backslash_w() {
        let text = r"var \w+";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\w+");
        assert!(result.unwrap().contains(r"\w+"));
    }

    #[test]
    fn detect_regex_backslash_w_star() {
        let text = r"prefix\w*";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\w*");
        assert!(result.unwrap().contains(r"\w*"));
    }

    #[test]
    fn detect_regex_backslash_t() {
        let text = r"col1\tcol2";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\t");
        assert!(result.unwrap().contains(r"\t"));
    }

    #[test]
    fn detect_regex_backslash_r() {
        let text = r"line\r\n";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\r");
        let found = result.unwrap();
        assert!(found.contains(r"\r"));
        assert!(found.contains(r"\n"));
    }

    #[test]
    fn detect_regex_backslash_s_plus() {
        let text = r"word\s+word";
        let result = detect_regex_patterns(text);
        assert!(result.is_some(), "should detect \\s+");
        assert!(result.unwrap().contains(r"\s+"));
    }

    #[test]
    fn detect_regex_deduplicates_repeated_tokens() {
        // \s* appears three times, but should be listed only once.
        let text = r"a\s*b\s*c\s*d";
        let result = detect_regex_patterns(text);
        assert!(result.is_some());
        let found = result.unwrap();
        // Count occurrences of \s* in the output.
        let count = found.matches(r"\s*").count();
        assert_eq!(count, 1, "\\s* should appear exactly once, got: {found}");
    }
}
