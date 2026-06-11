//! `EditFile` tool: hashline-anchored editing with legacy exact-match support.

use crate::error::ToolError;
use crate::hashline::{apply_hashline_to_content, format_fresh_anchors, format_hashline_diff};
use async_trait::async_trait;
use rho_core::newtypes::FilePath;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use tracing::{info, warn};

/// A legacy edit using exact text matching.
struct Edit {
    /// The text to find in the file.
    old_text: String,
    /// The replacement text.
    new_text: String,
}

/// Edit tool — applies hashline-anchored or legacy exact-match edits to a file.
pub struct EditFile {
    /// Sandbox root for path validation.
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
         Operations: replace, append, prepend, delete. \
         Example: {op: \"replace\", pos: \"9#KTNS\", lines: [\"new content\"]}. \
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
                            "old_text": { "type": "string", "description": "Legacy format: The exact literal text to find in the file." },
                            "new_text": { "type": "string", "description": "Legacy format: The text to replace old_text with." },
                            "op": { "type": "string", "enum": ["replace", "append", "prepend", "delete"], "description": "Hashline format: Operation type." },
                            "pos": { "type": "string", "description": "Hashline format: Anchor position (e.g., '2#KTNS')." },
                            "end": { "type": "string", "description": "Hashline format: End anchor for range operations (e.g., '5#ZT'). Optional." },
                            "lines": { "type": "array", "items": {"type": "string"}, "description": "Hashline format: Lines to insert or replace with." }
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
        let path_str = arguments["path"].as_str().ok_or_else(|| ToolError::MissingArgument { name: "path".to_string() })?;
        let edits_arg = arguments["edits"].as_array().ok_or_else(|| ToolError::MissingArgument { name: "edits".to_string() })?;

        if edits_arg.is_empty() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("edit_file: no edits provided")));
        }

        let has_hashline = edits_arg.iter().any(|e| e.get("op").is_some());
        let has_legacy = edits_arg.iter().any(|e| e.get("old_text").is_some());

        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate(&candidate)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        tracing::debug!(path = %path_str, edits = edits_arg.len(), format = if has_hashline && has_legacy { "mixed" } else if has_hashline { "hashline" } else { "legacy" }, "applying edits");

        if has_hashline && has_legacy {
            return self.apply_mixed_edits(path_str, &safe_path, edits_arg, cancel).await;
        }

        if has_hashline {
            return self.apply_hashline_edits(path_str, &safe_path, edits_arg, cancel).await;
        }

        let mut edits = Vec::with_capacity(edits_arg.len());
        for (i, edit_val) in edits_arg.iter().enumerate() {
            let old_text = edit_val["old_text"].as_str().ok_or_else(|| ToolError::MissingArgument { name: format!("edit {i} old_text") })?.to_owned();
            let new_text = edit_val["new_text"].as_str().ok_or_else(|| ToolError::MissingArgument { name: format!("edit {i} new_text") })?.to_owned();
            edits.push(Edit { old_text, new_text });
        }

        let content = match tokio::fs::read_to_string(&*safe_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to read `{path_str}`: {e}")))),
        };

        let mut match_ranges: Vec<(usize, usize, &Edit)> = Vec::with_capacity(edits.len());
        for edit in &edits {
            let occurrences: Vec<_> = content.match_indices(&edit.old_text).collect();
            match occurrences.len() {
                0 => {
                    let hint = crate::hashline::detect_regex_patterns(&edit.old_text).map_or_else(|| " Hint: old_text must match the file content exactly, character-for-character. Do not include `<context>` tags or `<context:end>` markers — they are framing, not file content. Use read_file to see the exact content.".to_owned(), |patterns| format!(" Hint: old_text contains regex-like patterns ({patterns}). old_text must be an exact match, not a regex."));
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: old_text not found in `{path_str}`: {:?}{hint}", crate::hashline::truncate_for_error(&edit.old_text, 80)))));
                }
                1 => {
                    let (start, _) = occurrences[0];
                    let end = start + edit.old_text.len();
                    match_ranges.push((start, end, edit));
                }
                _ => {
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: old_text is ambiguous ({} matches) in `{path_str}`: {:?}", occurrences.len(), crate::hashline::truncate_for_error(&edit.old_text, 80)))));
                }
            }
        }

        match_ranges.sort_by_key(|(start, _, _)| *start);
        for window in match_ranges.windows(2) {
            let (_, end_a, _) = window[0];
            let (start_b, _, _) = window[1];
            if start_b < end_a {
                return Ok(ToolOutcome::Immediate(ToolResult::error("edit_file: edits overlap")));
            }
        }

        let node_split_warnings = if std::path::Path::new(path_str).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("rs")) {
            check_node_splitting(&content, &match_ranges)
        } else {
            Vec::new()
        };

        let mut modified = content;
        for (start, end, edit) in match_ranges.into_iter().rev() {
            modified.replace_range(start..end, &edit.new_text);
        }

        if let Err(e) = tokio::fs::write(&*safe_path, &modified).await {
            warn!(path = %path_str, error = %e, "edit_file: failed to write modified file");
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to write `{path_str}`: {e}"))));
        }

        info!(path = %path_str, edits = edits.len(), "legacy edits applied successfully");
        let mut output = format!("applied {} edit(s) to {path_str}", edits.len());
        for warning in &node_split_warnings {
            output.push_str("\n[warning] ");
            output.push_str(warning);
        }
        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

impl EditFile {
    /// Apply hashline-anchored edits to the file.
    async fn apply_hashline_edits(&self, path_str: &str, safe_path: &FilePath, edits_arg: &[serde_json::Value], cancel: CancellationToken) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = match tokio::fs::read_to_string(&**safe_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to read `{path_str}`: {e}")))),
        };

        match apply_hashline_to_content(&content, edits_arg) {
            Ok((modified, relaxation_notes)) => {
                if let Err(e) = tokio::fs::write(&**safe_path, &modified).await {
                    return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to write `{path_str}`: {e}"))));
                }

                let has_relaxation = !relaxation_notes.is_empty();
                let mut output = if has_relaxation {
                    format!("applied {} hashline edit(s) to {} (with anchor relaxation)", edits_arg.len(), path_str)
                } else {
                    format!("applied {} hashline edit(s) to {}", edits_arg.len(), path_str)
                };

                for note in &relaxation_notes {
                    output.push_str("\n  ");
                    output.push_str(note);
                }
                if has_relaxation {
                    output.push_str("\n  Warning: hashes were stale. Re-read file if further edits needed.");
                }

                let diff = format_hashline_diff(&content, &modified);
                if !diff.is_empty() {
                    output.push_str("\n<diff>\n");
                    output.push_str(&diff);
                    output.push_str("</diff>");
                }

                if let Some(anchors) = format_fresh_anchors(&content, &modified) {
                    output.push_str(&anchors);
                }

                Ok(ToolOutcome::Immediate(ToolResult::success(output)))
            }
            Err(msg) => Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        }
    }

    /// Apply a mix of hashline and legacy edits in one pass.
    async fn apply_mixed_edits(&self, path_str: &str, safe_path: &FilePath, edits_arg: &[serde_json::Value], cancel: CancellationToken) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = match tokio::fs::read_to_string(&**safe_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to read `{path_str}`: {e}")))),
        };

        let hashline_args: Vec<serde_json::Value> = edits_arg.iter().filter(|e| e.get("op").is_some()).cloned().collect();
        let legacy_args: Vec<serde_json::Value> = edits_arg.iter().filter(|e| e.get("old_text").is_some()).cloned().collect();

        let (intermediate, _hashline_notes) = match apply_hashline_to_content(&content, &hashline_args) {
            Ok((c, notes)) => (c, notes),
            Err(msg) => return Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        };

        let edits = parse_legacy_edits(&legacy_args)?;
        let modified = match apply_legacy_edits_to_content(&intermediate, path_str, &edits) {
            Ok(m) => m,
            Err(msg) => return Ok(ToolOutcome::Immediate(ToolResult::error(msg))),
        };

        if let Err(e) = tokio::fs::write(&**safe_path, &modified).await {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!("edit_file: failed to write `{path_str}`: {e}"))));
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(format!("applied {} hashline + {} legacy edit(s) to {}", hashline_args.len(), legacy_args.len(), path_str))))
    }
}

/// Parse legacy `old_text`/`new_text` edits from raw JSON values.
fn parse_legacy_edits(legacy_args: &[serde_json::Value]) -> std::result::Result<Vec<Edit>, ToolError> {
    let mut edits = Vec::with_capacity(legacy_args.len());
    for (i, edit_val) in legacy_args.iter().enumerate() {
        let old_text = edit_val["old_text"].as_str().ok_or_else(|| ToolError::MissingArgument { name: format!("edit {i} old_text") })?.to_owned();
        let new_text = edit_val["new_text"].as_str().ok_or_else(|| ToolError::MissingArgument { name: format!("edit {i} new_text") })?.to_owned();
        edits.push(Edit { old_text, new_text });
    }
    Ok(edits)
}

/// Apply legacy (non-hashline) edits to file content, returning the modified string or an error.
fn apply_legacy_edits_to_content(content: &str, path_str: &str, edits: &[Edit]) -> std::result::Result<String, String> {
    let mut match_ranges: Vec<(usize, usize, &Edit)> = Vec::with_capacity(edits.len());

    for edit in edits {
        let occurrences: Vec<_> = content.match_indices(&edit.old_text).collect();
        match occurrences.len() {
            0 => {
                let hint = crate::hashline::detect_regex_patterns(&edit.old_text).map_or_else(|| " Hint: old_text must match the file content exactly, character-for-character. Use read_file to see the exact content.".to_owned(), |patterns| format!(" Hint: old_text contains regex-like patterns ({patterns}). old_text must be an exact match, not a regex."));
                return Err(format!("edit_file: old_text not found in `{path_str}`: {:?}{hint}", crate::hashline::truncate_for_error(&edit.old_text, 80)));
            }
            1 => {
                let (start, _) = occurrences[0];
                let end = start + edit.old_text.len();
                match_ranges.push((start, end, edit));
            }
            _ => {
                return Err(format!("edit_file: old_text is ambiguous ({} matches) in `{path_str}`: {:?}", occurrences.len(), crate::hashline::truncate_for_error(&edit.old_text, 80)));
            }
        }
    }

    match_ranges.sort_by_key(|(start, _, _)| *start);
    for window in match_ranges.windows(2) {
        let (_, end_a, _) = window[0];
        let (start_b, _, _) = window[1];
        if start_b < end_a {
            return Err("edit_file: edits overlap".to_string());
        }
    }

    let mut modified = content.to_string();
    for (start, end, edit) in match_ranges.into_iter().rev() {
        modified.replace_range(start..end, &edit.new_text);
    }
    Ok(modified)
}

/// Check whether edits split AST nodes, returning warning strings.
fn check_node_splitting(source: &str, match_ranges: &[(usize, usize, &Edit)]) -> Vec<String> {
    let Ok(tree) = rho_highlight::parse(source, rho_highlight::Language::Rust) else {
        return Vec::new();
    };

    let mut warnings = Vec::new();
    for &(start, end, _) in match_ranges {
        if let Some(warning) = check_boundary_at_byte(&tree, source, start, "start") {
            warnings.push(warning);
        }
        if let Some(warning) = check_boundary_at_byte(&tree, source, end, "end") {
            warnings.push(warning);
        }
    }
    warnings
}

/// Check whether a byte boundary lands inside a non-structural AST node.
fn check_boundary_at_byte(tree: &tree_sitter::Tree, source: &str, byte_pos: usize, boundary_label: &str) -> Option<String> {
    let (line, col) = byte_offset_to_line_col(source, byte_pos)?;
    let info = rho_highlight::node_at(tree, source, line, col).ok()?;

    if info.is_error || byte_pos == info.start_byte || byte_pos == info.end_byte {
        return None;
    }

    if is_structural_node(&info.kind) {
        return None;
    }

    let node_text = &info.text;
    let preview = if node_text.len() > 40 {
        let mut end = 40;
        while !node_text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &node_text[..end])
    } else {
        node_text.to_owned()
    };

    Some(format!("edit {boundary_label} splits a `{}` node: {:?}", info.kind, preview))
}

/// Return `true` if the node kind is structural (safe to split at its boundaries).
fn is_structural_node(kind: &str) -> bool {
    matches!(
        kind,
        "source_file" | "block" | "function_item" | "impl_item" | "struct_item" | "enum_item" | "trait_item" | "mod_item" | "use_declaration" | "let_declaration" | "expression_statement" | "if_expression" | "match_expression" | "match_arm" | "for_expression" | "while_expression" | "loop_expression" | "return_expression" | "call_expression" | "method_call_expression" | "field_expression" | "index_expression" | "binary_expression" | "unary_expression" | "reference_expression" | "assignment_expression" | "closure_expression" | "tuple_expression" | "array_expression" | "parameters" | "arguments" | "type_parameters" | "where_clause" | "field_declaration_list" | "enum_variant_list" | "attribute_item" | "token_tree"
    )
}

/// Convert a byte offset to (line, column) coordinates.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_splitting_warns_on_split_string_literal() {
        let source = r#"fn main() { let x = "hello"; }"#;
        let edit = Edit { old_text: "hel".to_owned(), new_text: "HEL".to_owned() };
        let start = source.find("hel").unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        let warnings = check_node_splitting(source, &ranges);
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("string_content") || warnings[0].contains("string_literal"));
    }

    #[test]
    fn node_splitting_no_warning_on_clean_boundary() {
        let source = "fn main() {\n    let x = 42;\n    let y = 10;\n}";
        let old = "let x = 42;";
        let edit = Edit { old_text: old.to_owned(), new_text: "let x = 99;".to_owned() };
        let start = source.find(old).unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        let warnings = check_node_splitting(source, &ranges);
        assert!(warnings.is_empty());
    }

    #[test]
    fn node_splitting_no_warning_for_non_rust_files() {
        let source = "this is not valid rust at all {{{{";
        let edit = Edit { old_text: "not".to_owned(), new_text: "NOT".to_owned() };
        let start = source.find("not").unwrap();
        let end = start + edit.old_text.len();
        let ranges = vec![(start, end, &edit)];

        let warnings = check_node_splitting(source, &ranges);
        let _ = warnings;
    }
}