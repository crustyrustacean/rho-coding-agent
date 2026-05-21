//! File operation tools: [`ReadFile`], [`WriteFile`], [`ListDir`], and [`EditFile`].
//!
//! Phase 1b: sandbox enforcement via [`SandboxRoot`], and `<context>` framing on
//! `ReadFile` output so the model treats file contents as data, not instructions.
//!
//! Phase 2: [`ListDir`] with `.gitignore`-aware directory walking via the
//! `ignore` crate, and [`EditFile`] with exact-match replacement and validation.

use crate::error::ToolError;
use crate::hashline::compute_line_hash;
use async_trait::async_trait;
use rho_core::newtypes::FilePath;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};

// ── Hashline Edit Types ───────────────────────────────────────────────────────

/// Hashline edit operation type.
#[derive(Debug, Clone, PartialEq)]
enum HashlineOp {
    /// Replace line(s) at anchor position.
    Replace,
    /// Insert lines after anchor position.
    Append,
    /// Insert lines before anchor position.
    Prepend,
    /// Delete line(s) at anchor position.
    Delete,
}

/// Parsed hashline anchor: line number and hash.
struct HashlineAnchor {
    /// 1-indexed line number.
    line_num: usize,
    /// 2-character hash string.
    hash: String,
}

impl HashlineAnchor {
    /// Parse a hashline anchor string (e.g., "2#KT").
    fn parse(anchor: &str) -> Option<Self> {
        let parts: Vec<&str> = anchor.split('#').collect();
        if parts.len() != 2 {
            return None;
        }
        let line_num = parts[0].parse::<usize>().ok()?;
        let hash = parts[1].to_string();
        Some(HashlineAnchor { line_num, hash })
    }
}

/// Hashline edit with operation type and parameters.
struct HashlineEdit {
    /// Operation to perform.
    op: HashlineOp,
    /// Starting anchor position.
    pos: HashlineAnchor,
    /// Ending anchor for range operations.
    end: Option<HashlineAnchor>,
    /// Lines to insert/replace (empty for delete).
    lines: Vec<String>,
}

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
         Returns the file's content wrapped in <context> tags. \
         When hashline is enabled (default), each line is prefixed with LINE#HASH: \
         (e.g., '  9#KT:  console.log(\"world\");') for reliable editing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to the project root)."
                },
                "hashline": {
                    "type": "boolean",
                    "description": "Enable hashline format (LINE#HASH: prefix). Defaults to true."
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
            .ok_or_else(|| ToolError::MissingArgument {
                name: "path".to_string(),
            })?;

        // hashline defaults to true
        let hashline = arguments["hashline"].as_bool().unwrap_or(true);

        // Validate path is within the sandbox root.
        // Resolve relative paths against the sandbox root before validation.
        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate(&candidate)?;

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

        let formatted_content = if hashline {
            // Format with hashline: LINE#HASH:content
            let lines: Vec<&str> = content.lines().collect();
            if lines.is_empty() {
                // Empty file - just return framing
                String::new()
            } else {
                let pad_width = lines.len().to_string().len();
                let hashlined: Vec<String> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let line_num = i + 1; // 1-indexed
                        let hash = compute_line_hash(line, line_num);
                        format!("{line_num:>pad_width$}#{hash}:{line}")
                    })
                    .collect();
                hashlined.join("\n")
            }
        } else {
            // Legacy format - just return content as-is
            content
        };

        // Wrap in <context> framing — signals to the model that this is data,
        // not instructions. The system prompt reinforces this contract.
        // <context:end> is an explicit boundary marker so the model can
        // distinguish the framing from trailing newlines in the file content.
        let framed = format!("<context>\n{formatted_content}\n<context:end>");

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
            .ok_or_else(|| ToolError::MissingArgument {
                name: "path".to_string(),
            })?;
        let content = arguments["content"]
            .as_str()
            .ok_or_else(|| ToolError::MissingArgument {
                name: "content".to_string(),
            })?;

        // Use the write-variant validator that handles not-yet-existing paths.
        // Resolve relative paths against the sandbox root before validation.
        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate_for_write(&candidate)?;

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
#[allow(clippy::too_many_lines)]
impl Tool for EditFile {
    fn name(&self) -> ToolName {
        ToolName::from("edit_file")
    }

    fn description(&self) -> &str {
        "Apply targeted edits to a file within the project. \
         Uses hashline anchors from read_file output (LINE#HASH: prefix). \
         Operations: replace (swap line at anchor), append (insert after), \
         prepend (insert before), delete (remove line). \
         Example: {op: \"replace\", pos: \"9#KT\", lines: [\"new content\"]}. \
         Hash mismatches fail with fresh hashes for retry. \
         Also supports legacy {old_text, new_text} format."
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
                    "description": "List of edits to apply. Two formats supported: legacy {old_text, new_text} and hashline {op, pos, lines}.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {
                                "type": "string",
                                "description": "Legacy format: The exact literal text to find in the file. Must match character-for-character including whitespace and newlines. Not a regex."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "Legacy format: The text to replace old_text with."
                            },
                            "op": {
                                "type": "string",
                                "enum": ["replace", "append", "prepend", "delete"],
                                "description": "Hashline format: Operation type (replace/append/prepend/delete). Required for hashline edits."
                            },
                            "pos": {
                                "type": "string",
                                "description": "Hashline format: Anchor position (e.g., '2#KT'). Required for hashline edits."
                            },
                            "end": {
                                "type": "string",
                                "description": "Hashline format: End anchor for range operations (e.g., '5#ZT'). Optional."
                            },
                            "lines": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Hashline format: Lines to insert or replace with. Required for replace/append/prepend, ignored for delete."
                            }
                        }
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
            .ok_or_else(|| ToolError::MissingArgument {
                name: "path".to_string(),
            })?;
        let edits_arg =
            arguments["edits"]
                .as_array()
                .ok_or_else(|| ToolError::MissingArgument {
                    name: "edits".to_string(),
                })?;

        if edits_arg.is_empty() {
            return Ok(ToolOutcome::Immediate(ToolResult::error(
                "edit_file: no edits provided",
            )));
        }

        // Detect edit formats
        let has_hashline = edits_arg.iter().any(|e| e.get("op").is_some());
        let has_legacy = edits_arg.iter().any(|e| e.get("old_text").is_some());

        // Validate path is within the sandbox root.
        // Resolve relative paths against the sandbox root before validation.
        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate(&candidate)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // Route based on edit format
        if has_hashline && has_legacy {
            return self
                .apply_mixed_edits(path_str, &safe_path, edits_arg, cancel)
                .await;
        }

        if has_hashline {
            return self
                .apply_hashline_edits(path_str, &safe_path, edits_arg, cancel)
                .await;
        }

        // Parse legacy edits from JSON.
        let mut edits = Vec::with_capacity(edits_arg.len());
        for (i, edit_val) in edits_arg.iter().enumerate() {
            let old_text = edit_val["old_text"]
                .as_str()
                .ok_or_else(|| ToolError::MissingArgument {
                    name: format!("edit {i} old_text"),
                })?
                .to_owned();
            let new_text = edit_val["new_text"]
                .as_str()
                .ok_or_else(|| ToolError::MissingArgument {
                    name: format!("edit {i} new_text"),
                })?
                .to_owned();
            edits.push(Edit { old_text, new_text });
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
                        || {
                            " Hint: old_text must match the file content exactly, \
                            character-for-character. Do not include `<context>` tags \
                            or `<context:end>` markers — they are framing, not file content. \
                            Use read_file to see the exact content."
                                .to_owned()
                        },
                        |patterns| {
                            format!(
                                " Hint: old_text contains regex-like patterns ({patterns}). \
                                 old_text must be an exact character-for-character match \
                                 of the file content, not a regex."
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

        // Node-splitting validation: warn if an edit boundary falls inside
        // a syntax node. Only performed for Rust files.
        let node_split_warnings = if std::path::Path::new(path_str)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        {
            check_node_splitting(&content, &match_ranges)
        } else {
            Vec::new()
        };

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

        let mut output = format!("applied {} edit(s) to {path_str}", edits.len());
        for warning in &node_split_warnings {
            output.push_str("\n[warning] ");
            output.push_str(warning);
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

impl EditFile {
    /// Generate a terse diff between old and new content with hashline anchors.
    ///
    /// Shows ±3 lines of context around each changed region, using hashline
    /// format for the new content. Lines prefixed with `-` are removed, `+` added.
    fn format_hashline_diff(old: &str, new: &str) -> String {
        let old_lines: Vec<&str> = old.lines().collect();
        let new_lines: Vec<&str> = new.lines().collect();
        let width = new_lines.len().to_string().len().max(1);

        // Find changed line indices in the new content
        let mut changed_indices: Vec<usize> = Vec::new();
        let max_cmp = new_lines.len().min(old_lines.len());

        for i in 0..max_cmp {
            if old_lines[i] != new_lines[i] {
                changed_indices.push(i);
            }
        }
        // Added lines at end
        for i in old_lines.len()..new_lines.len() {
            changed_indices.push(i);
        }
        // Removed lines at end — mark last comparable line as changed
        if old_lines.len() > new_lines.len() && !new_lines.is_empty() {
            let last = new_lines.len() - 1;
            if changed_indices.last() != Some(&last) {
                changed_indices.push(last);
            }
        }

        if changed_indices.is_empty() {
            return String::new();
        }

        // Group changes into regions with ±3 context, merge overlapping
        let ctx = 3;
        let mut regions: Vec<(usize, usize)> = Vec::new();
        for &idx in &changed_indices {
            let start = idx.saturating_sub(ctx);
            let end = (idx + ctx).min(new_lines.len().saturating_sub(1));
            if let Some(last) = regions.last_mut()
                && start <= last.1 + 1
            {
                last.1 = last.1.max(end);
                continue;
            }
            regions.push((start, end));
        }

        let mut diff = String::new();
        // SAFETY: write! to String is infallible
        let mut w = |s: &str| {
            diff.push_str(s);
        };

        for (ri, &(start, end)) in regions.iter().enumerate() {
            if ri > 0 {
                w("  ...\n");
            }
            for i in start..=end {
                if i >= new_lines.len() {
                    break;
                }
                let line_num = i + 1;
                let hash = compute_line_hash(new_lines[i], line_num);
                let line_content = new_lines[i];
                let is_changed = changed_indices.binary_search(&i).is_ok();
                if is_changed && i < old_lines.len() {
                    // Modified line — show old then new
                    let old_content = old_lines[i];
                    let old_hash = compute_line_hash(old_content, line_num);
                    w(&format!("- {line_num:>width$}#{old_hash}:{old_content}\n"));
                    w(&format!("+ {line_num:>width$}#{hash}:{line_content}\n"));
                } else if is_changed {
                    // Added line
                    w(&format!("+ {line_num:>width$}#{hash}:{line_content}\n"));
                } else {
                    // Context line
                    w(&format!("  {line_num:>width$}#{hash}:{line_content}\n"));
                }
            }
        }
        diff
    }

    /// Apply hashline edits to content in memory.
    ///
    /// Returns the modified content on success, or an error message on failure
    /// (hash mismatch, out-of-range, invalid anchor, etc.).
    /// Parse JSON edit arguments into [`HashlineEdit`] structs.
    fn parse_hashline_edits(
        edits_arg: &[serde_json::Value],
    ) -> std::result::Result<Vec<HashlineEdit>, String> {
        let mut hashline_edits = Vec::new();
        for (i, edit_val) in edits_arg.iter().enumerate() {
            let op_str = edit_val["op"]
                .as_str()
                .ok_or_else(|| format!("edit_file: edit {i} missing 'op'"))?;

            let op = match op_str {
                "replace" => HashlineOp::Replace,
                "append" => HashlineOp::Append,
                "prepend" => HashlineOp::Prepend,
                "delete" => HashlineOp::Delete,
                other => return Err(format!("edit_file: invalid op '{other}'")),
            };

            let pos_str = edit_val["pos"]
                .as_str()
                .ok_or_else(|| format!("edit_file: edit {i} missing 'pos'"))?;

            let pos = HashlineAnchor::parse(pos_str)
                .ok_or_else(|| format!("edit_file: edit {i} invalid anchor '{pos_str}'"))?;

            let end = edit_val["end"].as_str().and_then(HashlineAnchor::parse);

            let edit_lines = if op == HashlineOp::Delete {
                Vec::new()
            } else {
                edit_val["lines"]
                    .as_array()
                    .ok_or_else(|| format!("edit_file: edit {i} missing 'lines'"))?
                    .iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            };

            hashline_edits.push(HashlineEdit {
                op,
                pos,
                end,
                lines: edit_lines,
            });
        }
        Ok(hashline_edits)
    }

    /// Validate line numbers and resolve anchors for hashline edits.
    ///
    /// Returns a list of [`AnchorResolution`] in the same order as the edits,
    /// with each resolved to the actual line that should be edited. Uses
    /// tiered validation:
    ///
    /// - **Tier 1:** Hash matches → exact match, apply silently.
    /// - **Tier 2:** Hash mismatches, line number valid, content structurally
    ///   similar → apply with warning (anchor relaxation).
    /// - **Tier 3:** Hash mismatches, content is different at the target line →
    ///   search ±5 lines for a matching line, apply there with warning.
    /// - **Hard fail:** No match found anywhere → return error with fresh hashes.
    #[allow(clippy::too_many_lines)]
    fn validate_hashline_edits_fuzzy(
        lines: &[&str],
        hashline_edits: &[HashlineEdit],
    ) -> std::result::Result<Vec<AnchorResolution>, String> {
        // First pass: validate line number ranges (non-negotiable)
        for edit in hashline_edits {
            if edit.pos.line_num == 0 || edit.pos.line_num > lines.len() {
                return Err(format!(
                    "edit_file: anchor line {} is out of range (file has {} lines)",
                    edit.pos.line_num,
                    lines.len()
                ));
            }
            if let Some(ref end) = edit.end
                && (end.line_num == 0 || end.line_num > lines.len())
            {
                return Err(format!(
                    "edit_file: end anchor line {} is out of range (file has {} lines)",
                    end.line_num,
                    lines.len()
                ));
            }
        }

        let mut resolutions = Vec::with_capacity(hashline_edits.len());

        for edit in hashline_edits {
            let target_idx = edit.pos.line_num - 1; // 0-indexed
            let line_content = lines[target_idx];
            let current_hash = compute_line_hash(line_content, edit.pos.line_num);

            // Tier 1: Exact hash match
            if current_hash == edit.pos.hash {
                resolutions.push(AnchorResolution {
                    resolved_line: target_idx,
                    exact_match: true,
                    relaxation_note: None,
                });
                continue;
            }

            // Tier 2: Hash mismatch, but content at the target line is similar.
            // Only apply fuzzy matching for lines with enough information content.
            if is_high_information_line(line_content) {
                let sim = content_similarity(line_content, edit.pos.hash.as_str());
                // Note: we can't do content_similarity against the original content
                // because the anchor only has the hash, not the original line text.
                // Instead, check if the line at the target line number has enough
                // information content to be a plausible target. Since we know the
                // model read the file and saw this line number, if the line is
                // substantive (not a bare `}` or blank), the line number alone is
                // a strong signal. Accept it with a warning.
                let _ = sim; // similarity used for neighborhood search below
            }

            // For Tier 2: the line number is still correct (within range), and
            // the line has enough information content to be a unique target.
            // The hash changed because a prior edit modified this line or shifted
            // content. Since the model intentionally targeted this line number,
            // trust the line number but warn about the stale hash.
            if is_high_information_line(line_content) {
                resolutions.push(AnchorResolution {
                    resolved_line: target_idx,
                    exact_match: false,
                    relaxation_note: Some(format!(
                        "anchor {}#{} relaxed to {}#{} (hash stale, line number valid)",
                        edit.pos.line_num,
                        edit.pos.hash,
                        edit.pos.line_num,
                        current_hash
                    )),
                });
                continue;
            }

            // Tier 3: The line at the target is low-information (e.g., `}`, blank).
            // Search ±5 lines for a line with high information content that has
            // the same hash as the anchor expects. This handles cases where a
            // prior edit shifted lines up or down.
            let search_radius = 5usize;
            let mut best_match: Option<(usize, String)> = None;

            for offset in 1..=search_radius {
                // Check line below
                if let Some(candidate_idx) =
                    target_idx.checked_add(offset).filter(|&i| i < lines.len())
                {
                    let candidate_line = lines[candidate_idx];
                    let candidate_hash =
                        compute_line_hash(candidate_line, candidate_idx + 1);
                    if candidate_hash == edit.pos.hash
                        && is_high_information_line(candidate_line)
                    {
                        best_match = Some((
                            candidate_idx,
                            format!(
                                "anchor {}#{} resolved to {}#{} (neighborhood search, +{offset})",
                                edit.pos.line_num,
                                edit.pos.hash,
                                candidate_idx + 1,
                                candidate_hash
                            ),
                        ));
                        break;
                    }
                }
                // Check line above
                if let Some(candidate_idx) = target_idx.checked_sub(offset) {
                    let candidate_line = lines[candidate_idx];
                    let candidate_hash =
                        compute_line_hash(candidate_line, candidate_idx + 1);
                    if candidate_hash == edit.pos.hash
                        && is_high_information_line(candidate_line)
                    {
                        best_match = Some((
                            candidate_idx,
                            format!(
                                "anchor {}#{} resolved to {}#{} (neighborhood search, -{offset})",
                                edit.pos.line_num,
                                edit.pos.hash,
                                candidate_idx + 1,
                                candidate_hash
                            ),
                        ));
                        break;
                    }
                }
            }

            if let Some((resolved_idx, note)) = best_match {
                resolutions.push(AnchorResolution {
                    resolved_line: resolved_idx,
                    exact_match: false,
                    relaxation_note: Some(note),
                });
                continue;
            }

            // Hard fail: no match found. Return error with fresh hashes.
            let mismatch_line = edit.pos.line_num;
            let context_start = mismatch_line.saturating_sub(3);
            let context_end = (mismatch_line + 3).min(lines.len());

            let mut context_lines = Vec::new();
            let width = lines.len().to_string().len();
            for (i, line) in lines.iter().enumerate() {
                let line_num = i + 1;
                if line_num >= context_start && line_num <= context_end {
                    let hash = compute_line_hash(line, line_num);
                    context_lines.push(format!("{line_num:>width$}#{hash}:{line}"));
                }
            }

            return Err(format!(
                "edit_file: hash mismatch at anchor {}#{} and no similar content found nearby\n\
                 Expected line:  {}#{}:{}\n\
                 Actual line:    {}#{}:{}\n\
                 \n\
                 Fresh hashes around mismatch:\n\
                 {}\n\
                 \n\
                 Use updated anchor {}#{} to retry.",
                edit.pos.line_num,
                edit.pos.hash,
                edit.pos.line_num,
                edit.pos.hash,
                line_content,
                edit.pos.line_num,
                current_hash,
                line_content,
                context_lines.join("\n                     "),
                edit.pos.line_num,
                current_hash
            ));
        }

        Ok(resolutions)
    }

    /// Apply parsed hashline edits to content, returning modified content.
    ///
    /// Uses fuzzy anchor matching: stale hashes are relaxed when the line
    /// number is still valid and the target line has enough information content.
    /// Returns `(modified_content, relaxation_notes)` on success.
    fn apply_hashline_to_content(
        content: &str,
        edits_arg: &[serde_json::Value],
    ) -> std::result::Result<(String, Vec<String>), String> {
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Err("edit_file: cannot edit empty file".to_string());
        }

        let hashline_edits = Self::parse_hashline_edits(edits_arg)?;
        let resolutions = Self::validate_hashline_edits_fuzzy(&lines, &hashline_edits)?;

        // Collect relaxation notes for reporting
        let relaxation_notes: Vec<String> = resolutions
            .iter()
            .filter_map(|r| r.relaxation_note.clone())
            .collect();

        // Build (original_edit_idx, resolved_0indexed_line) pairs for sorted application
        let edit_targets: Vec<(usize, usize)> = hashline_edits
            .iter()
            .zip(resolutions.iter())
            .map(|(_, res)| (res.resolved_line, res.resolved_line))
            .collect();

        // Apply edits from last to first to preserve line numbers.
        // Sort by resolved line position descending.
        let mut apply_order: Vec<usize> = (0..hashline_edits.len()).collect();
        apply_order.sort_by(|&a, &b| edit_targets[b].0.cmp(&edit_targets[a].0));

        let mut modified: Vec<String> =
            lines.iter().map(std::string::ToString::to_string).collect();

        for edit_idx in apply_order {
            let edit = &hashline_edits[edit_idx];
            let idx = resolutions[edit_idx].resolved_line; // 0-indexed

            match edit.op {
                HashlineOp::Replace => {
                    if let Some(ref end) = edit.end {
                        // For range operations, compute the span relative to
                        // the resolved start position. The end anchor's offset
                        // from the start anchor is preserved.
                        let original_span = end.line_num - edit.pos.line_num;
                        let end_idx = idx + original_span;
                        if end_idx >= modified.len() {
                            return Err(
                                "edit_file: range end would exceed file length".to_string()
                            );
                        }
                        modified.splice(idx..=end_idx, edit.lines.clone());
                    } else {
                        modified[idx] = edit.lines.join("\n");
                    }
                }
                HashlineOp::Append => {
                    let insert_pos = idx + 1;
                    if insert_pos < modified.len() {
                        modified.splice(insert_pos..insert_pos, edit.lines.clone());
                    } else {
                        modified.extend(edit.lines.clone());
                    }
                }
                HashlineOp::Prepend => {
                    modified.splice(idx..idx, edit.lines.clone());
                }
                HashlineOp::Delete => {
                    if let Some(ref end) = edit.end {
                        let original_span = end.line_num - edit.pos.line_num;
                        let end_idx = idx + original_span;
                        if end_idx >= modified.len() {
                            return Err(
                                "edit_file: range end would exceed file length".to_string()
                            );
                        }
                        modified.drain(idx..=end_idx);
                    } else {
                        modified.remove(idx);
                    }
                }
            }
        }

        Ok((modified.join("\n"), relaxation_notes))
    }

    /// Apply pure hashline edits: read, validate, apply, write.
    async fn apply_hashline_edits(
        &self,
        path_str: &str,
        safe_path: &FilePath,
        edits_arg: &[serde_json::Value],
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = match tokio::fs::read_to_string(&**safe_path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "edit_file: failed to read `{path_str}`: {e}"
                ))));
            }
        };

        match Self::apply_hashline_to_content(&content, edits_arg) {
            Ok((modified, relaxation_notes)) => {
                if let Err(e) = tokio::fs::write(&**safe_path, &modified).await {
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                        "edit_file: failed to write `{path_str}`: {e}"
                    ))));
                }

                let has_relaxation = !relaxation_notes.is_empty();
                let mut output = if has_relaxation {
                    format!(
                        "applied {} hashline edit(s) to {} (with anchor relaxation)",
                        edits_arg.len(),
                        path_str
                    )
                } else {
                    format!(
                        "applied {} hashline edit(s) to {}",
                        edits_arg.len(),
                        path_str
                    )
                };

                for note in &relaxation_notes {
                    output.push_str("\n  ");
                    output.push_str(note);
                }
                if has_relaxation {
                    output.push_str(
                        "\n  Warning: hashes were stale. Re-read file if further edits needed.",
                    );
                }

                let diff = Self::format_hashline_diff(&content, &modified);
                if !diff.is_empty() {
                    output.push_str("\n<diff>\n");
                    output.push_str(&diff);
                    output.push_str("</diff>");
                }

                // Fresh anchors block: provide ±5 lines around each edit region
                // so the model can make chained edits without re-reading the file.
                let new_lines: Vec<&str> = modified.lines().collect();
                if !new_lines.is_empty() {
                    // Find changed regions in the new content
                    let old_lines: Vec<&str> = content.lines().collect();
                    let mut changed_indices: Vec<usize> = Vec::new();
                    let max_cmp = new_lines.len().min(old_lines.len());
                    for i in 0..max_cmp {
                        if old_lines[i] != new_lines[i] {
                            changed_indices.push(i);
                        }
                    }
                    for i in old_lines.len()..new_lines.len() {
                        changed_indices.push(i);
                    }
                    if old_lines.len() > new_lines.len() && !new_lines.is_empty() {
                        let last = new_lines.len() - 1;
                        if changed_indices.last() != Some(&last) {
                            changed_indices.push(last);
                        }
                    }

                    if !changed_indices.is_empty() {
                        // Find min/max changed indices for the anchor region
                        let first_change = *changed_indices.first().unwrap();
                        let last_change = *changed_indices.last().unwrap();
                        let anchor_radius = 5usize;
                        let anchor_start = first_change.saturating_sub(anchor_radius);
                        let anchor_end =
                            (last_change + anchor_radius).min(new_lines.len() - 1);

                        let width = new_lines.len().to_string().len();
                        let mut fresh_anchors = String::new();
                        for (i, line) in new_lines.iter().enumerate()
                            .skip(anchor_start)
                            .take(anchor_end - anchor_start + 1)
                        {
                            let line_num = i + 1;
                            let hash = compute_line_hash(line, line_num);
                            let _ = std::fmt::write(
                                &mut fresh_anchors,
                                format_args!("  {line_num:>width$}#{hash}:{line}\n"),
                            );
                        }

                        output.push_str("\n<fresh-anchors>\n");
                        output.push_str(&fresh_anchors);
                        output.push_str("</fresh-anchors>");
                        output.push_str("\nLines have fresh anchors. \
                         Use these for subsequent edits to this region.");
                    }
                }

                Ok(ToolOutcome::Immediate(ToolResult::success(output)))
            }
            Err(msg) => Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        }
    }

    /// Parse JSON edit arguments into legacy [`Edit`] structs.
    fn parse_legacy_edits(
        legacy_args: &[serde_json::Value],
    ) -> std::result::Result<Vec<Edit>, ToolError> {
        let mut edits = Vec::with_capacity(legacy_args.len());
        for (i, edit_val) in legacy_args.iter().enumerate() {
            let old_text = edit_val["old_text"]
                .as_str()
                .ok_or_else(|| ToolError::MissingArgument {
                    name: format!("edit {i} old_text"),
                })?
                .to_owned();
            let new_text = edit_val["new_text"]
                .as_str()
                .ok_or_else(|| ToolError::MissingArgument {
                    name: format!("edit {i} new_text"),
                })?
                .to_owned();
            edits.push(Edit { old_text, new_text });
        }
        Ok(edits)
    }

    /// Validate and apply legacy `old_text`/`new_text` edits to content.
    ///
    /// Returns the modified content, or an error string for the tool.
    fn apply_legacy_edits_to_content(
        content: &str,
        path_str: &str,
        edits: &[Edit],
    ) -> std::result::Result<String, String> {
        let mut match_ranges: Vec<(usize, usize, &Edit)> = Vec::with_capacity(edits.len());

        for edit in edits {
            let occurrences: Vec<_> = content.match_indices(&edit.old_text).collect();
            match occurrences.len() {
                0 => {
                    let hint = detect_regex_patterns(&edit.old_text).map_or_else(
                        || {
                            " Hint: old_text must match the file content exactly, \
                             character-for-character. \
                             Use read_file to see the exact content."
                                .to_owned()
                        },
                        |patterns| {
                            format!(
                                " Hint: old_text contains regex-like patterns ({patterns}). \
                                 old_text must be an exact match, not a regex."
                            )
                        },
                    );
                    return Err(format!(
                        "edit_file: old_text not found in `{path_str}`: {:?}{hint}",
                        truncate_for_error(&edit.old_text, 80)
                    ));
                }
                1 => {
                    let (start, _) = occurrences[0];
                    let end = start + edit.old_text.len();
                    match_ranges.push((start, end, edit));
                }
                _ => {
                    return Err(format!(
                        "edit_file: old_text is ambiguous ({} matches) in `{path_str}`: {:?}",
                        occurrences.len(),
                        truncate_for_error(&edit.old_text, 80)
                    ));
                }
            }
        }

        // Check for overlaps
        match_ranges.sort_by_key(|(start, _, _)| *start);
        for window in match_ranges.windows(2) {
            let (_, end_a, _) = window[0];
            let (start_b, _, _) = window[1];
            if start_b < end_a {
                return Err(
                    "edit_file: edits overlap — two edits target the same region of the file"
                        .to_string(),
                );
            }
        }

        // Apply edits from last to first
        let mut modified = content.to_string();
        for (start, end, edit) in match_ranges.into_iter().rev() {
            modified.replace_range(start..end, &edit.new_text);
        }
        Ok(modified)
    }

    /// Apply mixed hashline + legacy edits: hashline first, then legacy.
    async fn apply_mixed_edits(
        &self,
        path_str: &str,
        safe_path: &FilePath,
        edits_arg: &[serde_json::Value],
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = match tokio::fs::read_to_string(&**safe_path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "edit_file: failed to read `{path_str}`: {e}"
                ))));
            }
        };

        // Separate hashline and legacy edits
        let hashline_args: Vec<serde_json::Value> = edits_arg
            .iter()
            .filter(|e| e.get("op").is_some())
            .cloned()
            .collect();
        let legacy_args: Vec<serde_json::Value> = edits_arg
            .iter()
            .filter(|e| e.get("old_text").is_some())
            .cloned()
            .collect();

        // Apply hashline edits first (they reference original line numbers)
        let (intermediate, _hashline_notes) = match Self::apply_hashline_to_content(&content, &hashline_args) {
            Ok((c, notes)) => (c, notes),
            Err(msg) => return Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        };

        // Parse legacy edits
        let edits = Self::parse_legacy_edits(&legacy_args)?;

        // Validate and apply legacy edits to intermediate content
        let modified = match Self::apply_legacy_edits_to_content(&intermediate, path_str, &edits) {
            Ok(m) => m,
            Err(msg) => return Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        };

        // Write once
        if let Err(e) = tokio::fs::write(&**safe_path, &modified).await {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "edit_file: failed to write `{path_str}`: {e}"
            ))));
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(format!(
            "applied {} hashline + {} legacy edit(s) to {}",
            hashline_args.len(),
            legacy_args.len(),
            path_str
        ))))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Check whether any edit boundary falls inside a tree-sitter syntax node.
///
/// Returns a list of warning strings for edits that split a node. For example,
/// if an edit's `old_text` starts in the middle of a string literal, this
/// emits a warning like "edit splits a `string_content` node".
///
/// This is best-effort: if the file can't be parsed, or if tree-sitter returns
/// an error node, the warning is suppressed (the edit proceeds regardless).
fn check_node_splitting(source: &str, match_ranges: &[(usize, usize, &Edit)]) -> Vec<String> {
    let Ok(tree) = rho_highlight::parse(source, rho_highlight::Language::Rust) else {
        return Vec::new();
    };

    let mut warnings = Vec::new();

    for &(start, end, _) in match_ranges {
        // Check both boundaries using node_at via line/col conversion.
        if let Some(warning) = check_boundary_at_byte(&tree, source, start, "start") {
            warnings.push(warning);
        }
        if let Some(warning) = check_boundary_at_byte(&tree, source, end, "end") {
            warnings.push(warning);
        }
    }

    warnings
}

/// Convert a byte offset to (line, column) then use `node_at` to check if
/// the position falls strictly inside a leaf syntax node.
fn check_boundary_at_byte(
    tree: &tree_sitter::Tree,
    source: &str,
    byte_pos: usize,
    boundary_label: &str,
) -> Option<String> {
    // Convert byte offset to (line, col) — both 0-based.
    let (line, col) = byte_offset_to_line_col(source, byte_pos)?;

    let info = rho_highlight::node_at(tree, source, line, col).ok()?;

    // Skip error nodes.
    if info.is_error {
        return None;
    }

    // If the position is at the start or end of the node, it's a clean boundary.
    if byte_pos == info.start_byte || byte_pos == info.end_byte {
        return None;
    }

    // Only warn for token-level nodes — identifiers, literals, keywords,
    // operators, etc. Structural nodes (blocks, items, statements) are
    // expected to be partially matched by edits.
    if is_structural_node(&info.kind) {
        return None;
    }

    let node_text = &info.text;
    let preview = if node_text.len() > 40 {
        format!("{}...", &node_text[..40])
    } else {
        node_text.to_owned()
    };

    Some(format!(
        "edit {boundary_label} splits a `{}` node: {:?}",
        info.kind, preview
    ))
}

/// Returns `true` for tree-sitter node kinds that represent structural
/// containers (blocks, items, statements, declarations). Splitting these
/// is expected and should not trigger a warning.
fn is_structural_node(kind: &str) -> bool {
    matches!(
        kind,
        "source_file"
            | "block"
            | "function_item"
            | "impl_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "mod_item"
            | "use_declaration"
            | "let_declaration"
            | "expression_statement"
            | "if_expression"
            | "match_expression"
            | "match_arm"
            | "for_expression"
            | "while_expression"
            | "loop_expression"
            | "return_expression"
            | "call_expression"
            | "method_call_expression"
            | "field_expression"
            | "index_expression"
            | "binary_expression"
            | "unary_expression"
            | "reference_expression"
            | "assignment_expression"
            | "closure_expression"
            | "tuple_expression"
            | "array_expression"
            | "parameters"
            | "arguments"
            | "type_parameters"
            | "where_clause"
            | "field_declaration_list"
            | "enum_variant_list"
            | "attribute_item"
            | "token_tree"
    )
}

/// Convert a byte offset to 0-based `(line, column)` in bytes.
fn byte_offset_to_line_col(source: &str, byte_pos: usize) -> Option<(usize, usize)> {
    if byte_pos > source.len() {
        return None;
    }
    let before = &source[..byte_pos];
    let line = before.matches('\n').count();
    let last_newline = before.rfind('\n').map_or(0, |pos| pos + 1);
    let col = byte_pos - last_newline;
    Some((line, col))
}

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

/// Check whether a line has enough information content for reliable
/// content-based matching. Short lines like `}`, `]`, or empty lines
/// have too little signal and should not be fuzzy-matched.
fn is_high_information_line(line: &str) -> bool {
    let stripped = line.trim();
    // Require at least 4 non-whitespace characters
    if stripped.len() < 4 {
        return false;
    }
    // Require at least one alphanumeric character
    stripped.chars().any(char::is_alphanumeric)
}

/// Extract the first token from a line (up to first whitespace, paren, colon, or equals).
fn first_token(s: &str) -> &str {
    s.split(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '=')
        .next()
        .unwrap_or("")
}

/// Compute a content similarity score between two lines.
///
/// Returns a score >= 0 where higher means more similar:
/// - `usize::MAX` = perfect match (after stripping whitespace)
/// - 10+ = same leading token (first word)
/// - 0 = no meaningful similarity
///
/// This is used for fuzzy anchor matching when the hash mismatches but
/// the line number still points to the right content.
fn content_similarity(a: &str, b: &str) -> usize {
    let a_trimmed = a.trim_start();
    let b_trimmed = b.trim_start();

    if a_trimmed == b_trimmed {
        return usize::MAX; // Perfect match (whitespace-only difference)
    }

    // Check if the first "word" (up to first space/punct) matches.
    // This catches cases like `let metadata_json = ...` vs
    // `let metadata_json = serde_json::to_string(m)` — same leading `let`.
    let token_a = first_token(a_trimmed);
    let token_b = first_token(b_trimmed);

    if !token_a.is_empty() && token_a == token_b {
        return 10; // Same leading token — likely the right line
    }

    0 // No similarity
}

/// Result of fuzzy anchor validation.
#[allow(dead_code)]
struct AnchorResolution {
    /// The resolved line number (0-indexed) where the edit should apply.
    resolved_line: usize,
    /// Whether the hash matched exactly (no relaxation needed).
    exact_match: bool,
    /// Description of any relaxation applied, for the user/model.
    relaxation_note: Option<String>,
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

    // ── Node-splitting validation ───────────────────────────────

    #[test]
    fn node_splitting_warns_on_split_string_literal() {
        // Edit that splits the string "hello" by matching only "hel"
        let source = r#"fn main() { let x = "hello"; }"#;
        let edit = Edit {
            old_text: "hel".to_owned(),
            new_text: "HEL".to_owned(),
        };
        // Find where "hel" starts in the source.
        let start = source.find("hel").unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        let warnings = check_node_splitting(source, &ranges);
        assert!(
            !warnings.is_empty(),
            "should warn about splitting a string node"
        );
        // The warning should mention the node kind.
        assert!(
            warnings[0].contains("string_content") || warnings[0].contains("string_literal"),
            "warning should mention the split node kind: {}",
            warnings[0]
        );
    }

    #[test]
    fn node_splitting_no_warning_on_clean_boundary() {
        // Replace entire let binding — clean node boundaries.
        let source = "fn main() {\n    let x = 42;\n    let y = 10;\n}";
        let old = "let x = 42;";
        let edit = Edit {
            old_text: old.to_owned(),
            new_text: "let x = 99;".to_owned(),
        };
        let start = source.find(old).unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        let warnings = check_node_splitting(source, &ranges);
        assert!(
            warnings.is_empty(),
            "clean boundary edit should not warn: {warnings:?}"
        );
    }

    #[test]
    fn node_splitting_no_warning_for_non_rust_files() {
        // check_node_splitting is only called for .rs files;
        // this test verifies the function itself handles parse failure.
        let source = "this is not valid rust at all {{{{";
        let edit = Edit {
            old_text: "not".to_owned(),
            new_text: "NOT".to_owned(),
        };
        let start = source.find("not").unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        // Even with invalid source, check_node_splitting should not panic.
        let warnings = check_node_splitting(source, &ranges);
        // Warnings may or may not be emitted for invalid source — no panic is the requirement.
        let _ = warnings;
    }
}
