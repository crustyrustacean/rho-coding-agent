//! Entry outlining and summarization — reduced-fidelity text generation.
//!
//! Phase 2 produces tool-specific structural summaries. Each tool type has a
//! dedicated formatter that extracts key information from the result text,
//! producing concise outlines like `"read_file: src/parser.rs (342 lines)"`
//! instead of blindly truncating the content.
//!
//! The outline context carries optional metadata (tool name, arguments,
//! structured diagnostics) resolved by the caller (typically
//! `Session::outline_entry`).

use crate::diagnostic::{Diagnostic, DiagnosticLevel};
use crate::message::{ChatMessage, ContentBlock, ModelToolCall};
use crate::session::entry::{Entry, EntryPayload};
use crate::tool::ToolResultDetails;

// ── OutlineContext ─────────────────────────────────────────────────────────────

/// Optional context for tool-specific outlining.
///
/// Resolved by the caller from the session tree and `details_store`:
/// - `tool_name`: extracted from the parent `Assistant` entry's `tool_calls`
///   by matching the `Tool` message's `tool_call_id`.
/// - `tool_arguments`: the JSON arguments string from the same tool call.
/// - `details`: structured detail from the session's `details_store`
///   (e.g. `Diagnostics` for cargo tools, `FullOutput` for truncated reads).
#[derive(Clone, Debug, Default)]
pub(crate) struct OutlineContext {
    /// The name of the tool that produced this result, if known.
    pub tool_name: Option<crate::newtypes::ToolName>,
    /// The JSON arguments string from the tool call, if known.
    pub tool_arguments: Option<String>,
    /// Structured detail from the session's `details_store`, if available.
    pub details: Option<ToolResultDetails>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Generate a structural outline for an entry.
///
/// Uses tool-specific formatters when context is available, falling back to
/// content-based heuristics, and ultimately to truncation.
pub(crate) fn generate_outline(entry: &Entry, ctx: &OutlineContext) -> String {
    generate_reduced(entry, ctx, 200)
}

/// Generate a short summary for an entry.
///
/// Same logic as `generate_outline` but with a tighter budget.
pub(crate) fn generate_summary(entry: &Entry, ctx: &OutlineContext) -> String {
    generate_reduced(entry, ctx, 80)
}

// ── Core dispatch ─────────────────────────────────────────────────────────────

/// Dispatch to the appropriate formatter based on entry type and context.
fn generate_reduced(entry: &Entry, ctx: &OutlineContext, max_len: usize) -> String {
    match &entry.payload {
        EntryPayload::Message(msg) => match msg {
            ChatMessage::Tool { content, .. } => {
                let text = extract_text(content);
                let tool_name = ctx.tool_name.as_deref();
                let args = ctx.tool_arguments.as_deref();
                let diagnostics = ctx.details.as_ref().and_then(|d| match d {
                    ToolResultDetails::Diagnostics(diags) => Some(diags.as_slice()),
                    _ => None,
                });
                outline_tool_result(tool_name, args, diagnostics, &text, max_len)
            }
            ChatMessage::Assistant { tool_calls, .. } => outline_assistant(tool_calls, max_len),
            // User, System: truncate text content
            _ => {
                let text = extract_text_blocks(msg);
                truncate_with_ellipsis(&text, max_len)
            }
        },
        EntryPayload::CustomMessage { content, .. } => {
            let text = extract_text(content);
            truncate_with_ellipsis(&text, max_len)
        }
        // Non-message payloads: no meaningful outline
        _ => String::new(),
    }
}

// ── Tool-result formatters ────────────────────────────────────────────────────

/// Format a tool result using the best available formatter.
///
/// Priority:
/// 1. Tool-specific formatter (when tool name is known)
/// 2. Content-based heuristic (detect `<context>` framing, cargo output, etc.)
/// 3. Generic truncation
fn outline_tool_result(
    tool_name: Option<&str>,
    args: Option<&str>,
    diagnostics: Option<&[Diagnostic]>,
    text: &str,
    max_len: usize,
) -> String {
    let formatted = tool_name
        .and_then(|name| format_known_tool(name, args, diagnostics, text))
        .or_else(|| infer_tool_from_content(text));

    match formatted {
        Some(s) => truncate_with_ellipsis(&s, max_len),
        None => truncate_with_ellipsis(text, max_len),
    }
}

/// Format a known tool result structurally.
///
/// Returns `None` if the tool name is not one we have a formatter for.
fn format_known_tool(
    name: &str,
    args: Option<&str>,
    diagnostics: Option<&[Diagnostic]>,
    text: &str,
) -> Option<String> {
    match name {
        "read_file" => Some(format_read_file(args, text)),
        "write_file" => Some(format_write_file(args)),
        "edit_file" => Some(format_edit_file(args)),
        "list_dir" => Some(format_list_dir(args, text)),
        "run_command" => Some(format_run_command(args, text)),
        "cargo_check" => Some(format_cargo_check(diagnostics, text)),
        "cargo_clippy" => Some(format_cargo_clippy(diagnostics, text)),
        "cargo_test" => Some(format_cargo_test(text)),
        "cargo_fix" => Some(format_cargo_fix(diagnostics, text)),
        "rustc_explain" => Some(format_rustc_explain(args)),
        "crates_io_lookup" => Some(format_crates_io_lookup(args, text)),
        _ => None,
    }
}

/// Infer a tool type from result content patterns.
///
/// Used when the tool name is not available (e.g. extension tools).
fn infer_tool_from_content(text: &str) -> Option<String> {
    let trimmed = text.trim();

    if trimmed.starts_with("<context>") {
        return Some(infer_read_file_from_content(trimmed));
    }
    if trimmed.contains("<crate ") || trimmed.contains("<crate>") {
        return infer_crates_io_from_content(trimmed);
    }
    if trimmed.contains("test result:") || trimmed.contains("running ") {
        return infer_cargo_test_from_content(trimmed);
    }
    if trimmed.contains("error[E") || trimmed.contains("warning[") {
        return Some(infer_diagnostics_from_content(trimmed));
    }

    None
}

// ── Per-tool formatters ───────────────────────────────────────────────────────

/// `read_file`: extract path and line count from arguments or content.
fn format_read_file(args: Option<&str>, text: &str) -> String {
    let path = extract_json_string_arg(args, "path");
    // Strip `<context>` framing to count only actual file content lines.
    let inner = text
        .strip_prefix("<context>")
        .and_then(|s| s.strip_suffix("<context:end>"))
        .unwrap_or(text);
    let line_count = count_lines(inner.trim_start_matches('\n'));
    format!("read_file: {path} ({line_count} lines)")
}

/// Infer `read_file` from context-framed content when tool name is unknown.
fn infer_read_file_from_content(text: &str) -> String {
    let inner = text
        .strip_prefix("<context>")
        .and_then(|s| s.strip_suffix("<context:end>"))
        .unwrap_or(text);
    let line_count = count_lines(inner.trim_start_matches('\n'));
    format!("file content ({line_count} lines)")
}

/// `write_file`: extract path from arguments.
fn format_write_file(args: Option<&str>) -> String {
    let path = extract_json_string_arg(args, "path");
    format!("write_file: {path}")
}

/// `edit_file`: extract path from arguments.
fn format_edit_file(args: Option<&str>) -> String {
    let path = extract_json_string_arg(args, "path");
    format!("edit_file: {path}")
}

/// `list_dir`: extract path from arguments, entry count from content.
fn format_list_dir(args: Option<&str>, text: &str) -> String {
    let path = extract_json_string_arg(args, "path");
    let entry_count = count_dir_entries(text);
    format!("list_dir: {path} ({entry_count} entries)")
}

/// `run_command`: extract command from arguments, success/fail from content.
fn format_run_command(args: Option<&str>, text: &str) -> String {
    let command = extract_json_string_arg(args, "command");
    let is_error = text.contains("error") || text.contains("Error");
    let char_count = text.len();
    let status = if is_error { "failed" } else { "ok" };
    format!("run_command: \"{command}\" — {status} ({char_count} chars)")
}

/// `cargo_check`: count errors and warnings from diagnostics or content.
fn format_cargo_check(diagnostics: Option<&[Diagnostic]>, text: &str) -> String {
    let (errors, warnings) =
        diagnostics.map_or_else(|| infer_diagnostic_counts(text), count_diagnostic_levels);
    format!("cargo_check: {errors} errors, {warnings} warnings")
}

/// `cargo_clippy`: count errors and warnings from diagnostics or content.
fn format_cargo_clippy(diagnostics: Option<&[Diagnostic]>, text: &str) -> String {
    let (errors, warnings) =
        diagnostics.map_or_else(|| infer_diagnostic_counts(text), count_diagnostic_levels);
    format!("cargo_clippy: {errors} errors, {warnings} warnings")
}

/// `cargo_test`: extract pass/fail counts from content.
fn format_cargo_test(text: &str) -> String {
    let (passed, failed) = infer_test_counts(text);
    format!("cargo_test: {passed} passed, {failed} failed")
}

/// Infer `cargo_test` from content when tool name is unknown.
fn infer_cargo_test_from_content(text: &str) -> Option<String> {
    let (passed, failed) = infer_test_counts(text);
    (passed != 0 || failed != 0)
        .then_some(format!("test results: {passed} passed, {failed} failed"))
}

/// `cargo_fix`: summarize fix application from diagnostics or content.
fn format_cargo_fix(diagnostics: Option<&[Diagnostic]>, text: &str) -> String {
    let applied = diagnostics.map_or_else(
        || {
            text.lines()
                .filter(|l| l.contains("Fixing") || l.contains("Applying"))
                .count()
        },
        |diags| {
            diags
                .iter()
                .filter(|d| {
                    d.spans.iter().any(|s| {
                        s.suggestion.as_ref().is_some_and(|s| {
                            matches!(
                                s.applicability,
                                crate::diagnostic::SuggestionApplicability::MachineApplicable
                            )
                        })
                    })
                })
                .count()
        },
    );
    format!("cargo_fix: {applied} suggestions applied")
}

/// `rustc_explain`: extract error code from arguments.
fn format_rustc_explain(args: Option<&str>) -> String {
    let code = extract_json_string_arg(args, "code");
    format!("rustc_explain: {code}")
}

/// `crates_io_lookup`: format based on operation and content.
///
/// Operations: `search` (N results), `info` (name, version),
/// `versions` (N versions), `deps` (N dependencies).
fn format_crates_io_lookup(args: Option<&str>, text: &str) -> String {
    let operation = extract_json_string_arg(args, "operation");
    let crate_name = extract_json_string_arg(args, "crate_name");
    let query = extract_json_string_arg(args, "query");

    match operation.as_str() {
        "search" => {
            let count = count_tag_occurrences(text, "<crate ");
            let query_display = if query == "?" { &operation } else { &query };
            format!("crates_io search: \"{query_display}\" ({count} results)")
        }
        "info" => {
            let version = extract_attr_value(text, "version=");
            let name = if crate_name == "?" {
                extract_attr_value(text, "name=")
            } else {
                crate_name
            };
            format!("crates_io info: {name} v{version}")
        }
        "versions" => {
            let count = count_tag_occurrences(text, "<version ");
            format!("crates_io versions: {crate_name} ({count} versions)")
        }
        "deps" => {
            let count = count_tag_occurrences(text, "<dependency ");
            format!("crates_io deps: {crate_name} ({count} dependencies)")
        }
        _ => truncate_with_ellipsis(text, 200),
    }
}

/// Infer crates.io lookup from `<crate>` / `<version>` / `<dependency>` tags.
fn infer_crates_io_from_content(text: &str) -> Option<String> {
    if text.contains("<crate ") {
        if text.contains("<version ") {
            let count = count_tag_occurrences(text, "<version ");
            let name = extract_attr_value(text, "name=");
            return Some(format!("crates.io versions: {name} ({count} versions)"));
        }
        if text.contains("<dependency ") {
            let count = count_tag_occurrences(text, "<dependency ");
            let name = extract_attr_value(text, "name=");
            return Some(format!("crates.io deps: {name} ({count} dependencies)"));
        }
        let count = count_tag_occurrences(text, "<crate ");
        let version = extract_attr_value(text, "version=");
        if !version.is_empty() {
            return Some(format!("crates.io info: {version}"));
        }
        return Some(format!("crates.io search ({count} results)"));
    }
    None
}

/// Infer diagnostic summary from raw compiler output.
fn infer_diagnostics_from_content(text: &str) -> String {
    let (errors, warnings) = infer_diagnostic_counts(text);
    format!("compiler diagnostics: {errors} errors, {warnings} warnings")
}

// ── Assistant message formatter ───────────────────────────────────────────────

/// Format an assistant message that contains tool calls.
fn outline_assistant(tool_calls: &[ModelToolCall], max_len: usize) -> String {
    if tool_calls.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = tool_calls
        .iter()
        .map(|tc| {
            let args_summary = summarise_tool_args(&tc.function.arguments);
            format!("{}({})", tc.function.name, args_summary)
        })
        .collect();
    let formatted = format!("Requested: {}", parts.join(", "));
    truncate_with_ellipsis(&formatted, max_len)
}

// ── Content analysis helpers ──────────────────────────────────────────────────

/// Extract a JSON string argument by key from a JSON arguments string.
///
/// Returns the value if parsing succeeds and the key exists. Returns `"?"`
/// if parsing fails or the key is missing.
fn extract_json_string_arg(args: Option<&str>, key: &str) -> String {
    let Some(args) = args else {
        return "?".to_owned();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(args) else {
        return "?".to_owned();
    };
    val.get(key)
        .and_then(|v| v.as_str())
        .map_or_else(|| "?".to_owned(), std::borrow::ToOwned::to_owned)
}

/// Count non-empty lines in text.
fn count_lines(text: &str) -> usize {
    text.lines().filter(|l| !l.is_empty()).count()
}

/// Count directory entries from `list_dir` output (one per line).
fn count_dir_entries(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .count()
}

/// Count occurrences of an XML-like tag opening in text.
fn count_tag_occurrences(text: &str, tag: &str) -> usize {
    text.matches(tag).count()
}

/// Extract the value of the first XML-like attribute in text.
///
/// For `<foo name="bar" ...>`, searching for `name=` returns `bar`.
/// Returns an empty string if the attribute is not found.
fn extract_attr_value(text: &str, attr: &str) -> String {
    let Some(start) = text.find(attr) else {
        return String::new();
    };
    let rest = &text[start + attr.len()..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let end = rest.find('"').unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// Count diagnostic errors and warnings from structured diagnostics.
fn count_diagnostic_levels(diags: &[Diagnostic]) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for d in diags {
        match d.level {
            DiagnosticLevel::Error => errors += 1,
            DiagnosticLevel::Warning => warnings += 1,
            _ => {}
        }
    }
    (errors, warnings)
}

/// Infer error/warning counts from raw compiler output text.
fn infer_diagnostic_counts(text: &str) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("error") {
            errors += 1;
        }
        if trimmed.starts_with("warning") {
            warnings += 1;
        }
    }
    (errors, warnings)
}

/// Infer test pass/fail counts from cargo test output.
fn infer_test_counts(text: &str) -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;

    // Look for the summary line: `test result: <status>. <N> passed; <M> failed; ...`
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("test result: ") {
            for part in rest.split(';') {
                let part = part.trim();
                if let Some(n) = part
                    .strip_suffix(" passed")
                    .and_then(|s| s.split_whitespace().next_back())
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    passed = n;
                }
                if let Some(n) = part
                    .strip_suffix(" failed")
                    .and_then(|s| s.split_whitespace().next_back())
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    failed = n;
                }
            }
        }
    }

    if passed == 0 && failed == 0 {
        // Fall back to counting individual test result lines.
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.ends_with(" ... ok") {
                passed += 1;
            } else if trimmed.ends_with(" ... FAILED") {
                failed += 1;
            }
        }
    }

    (passed, failed)
}

/// Produce a short argument summary from a JSON arguments string.
///
/// Extracts up to 2 key-value pairs at 30 chars each.
fn summarise_tool_args(arguments: &str) -> String {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return truncate_with_ellipsis(arguments, 60);
    };
    let serde_json::Value::Object(map) = &val else {
        return truncate_with_ellipsis(&val.to_string(), 60);
    };
    let pairs: Vec<String> = map
        .iter()
        .take(2)
        .map(|(k, v)| {
            let v_str = match v {
                serde_json::Value::String(s) if s.len() > 30 => truncate_with_ellipsis(s, 30),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}={v_str}")
        })
        .collect();
    if pairs.is_empty() {
        truncate_with_ellipsis(&val.to_string(), 60)
    } else {
        pairs.join(", ")
    }
}

// ── Shared utilities ─────────────────────────────────────────────────────────

/// Truncate text to `max_len` characters at a UTF-8-safe boundary,
/// appending "…" if truncated.
fn truncate_with_ellipsis(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_owned();
    }
    let mut end = max_len;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Extract plain text from a slice of [`ContentBlock`]s.
fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.as_str(),
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Extract text from any [`ChatMessage`] variant (for truncation fallback).
fn extract_text_blocks(msg: &ChatMessage) -> String {
    match msg {
        ChatMessage::Tool { content, .. }
        | ChatMessage::User { content, .. }
        | ChatMessage::Assistant { content, .. }
        | ChatMessage::System { content, .. } => extract_text(content),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{DiagnosticSuggestion, SuggestionApplicability};
    use crate::message::{ChatMessage, ModelToolCall, ToolCallFunction};
    use crate::newtypes::{EntryId, ToolCallId, ToolName};
    use crate::session::entry::{Entry, EntryPayload, EntryResolution};
    use std::time::SystemTime;

    /// Helper: construct an [`OutlineContext`] with tool name only.
    fn ctx_named(name: &str) -> OutlineContext {
        OutlineContext {
            tool_name: Some(ToolName::from(name)),
            tool_arguments: None,
            details: None,
        }
    }

    /// Helper: construct an [`OutlineContext`] with tool name, args, and details.
    fn ctx_full(name: &str, args: &str, details: Option<ToolResultDetails>) -> OutlineContext {
        OutlineContext {
            tool_name: Some(ToolName::from(name)),
            tool_arguments: Some(args.to_owned()),
            details,
        }
    }

    fn test_entry(payload: EntryPayload) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    fn tool_entry(content: &str, call_id: &str) -> Entry {
        test_entry(EntryPayload::Message(ChatMessage::tool_result(
            ToolCallId::from(call_id),
            content,
        )))
    }

    fn assistant_entry(tool_calls: Vec<ModelToolCall>) -> Entry {
        test_entry(EntryPayload::Message(ChatMessage::Assistant {
            finish_reason: None,
            content: vec![],
            tool_calls,
        }))
    }

    fn tool_call(id: &str, name: &str, args: &str) -> ModelToolCall {
        ModelToolCall {
            id: ToolCallId::from(id),
            call_type: "function".to_owned(),
            function: ToolCallFunction {
                name: ToolName::from(name),
                arguments: args.to_owned(),
            },
        }
    }

    // ── read_file ──────────────────────────────────────────────────────────

    #[test]
    fn outline_read_file_shows_path_and_line_count() {
        let entry = tool_entry(
            "<context>\nfn main() {}\nfn helper() { println!(\"hi\"); }\n<context:end>",
            "call_1",
        );
        let ctx = ctx_full("read_file", r#"{"path":"src/main.rs"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "read_file: src/main.rs (2 lines)");
    }

    #[test]
    fn summary_read_file_shows_path_and_line_count() {
        let entry = tool_entry("<context>\nfn main() {}\n<context:end>", "call_1");
        let ctx = ctx_named("read_file");
        let summary = generate_summary(&entry, &ctx);
        assert!(summary.contains("read_file:"));
        assert!(summary.contains("lines)"));
    }

    #[test]
    fn outline_read_file_without_args_uses_placeholder() {
        let entry = tool_entry("<context>\nsome content\n<context:end>", "call_1");
        let ctx = ctx_named("read_file");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "read_file: ? (1 lines)");
    }

    #[test]
    fn outline_read_file_content_inference() {
        let entry = tool_entry("<context>\nline1\nline2\nline3\n<context:end>", "call_1");
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "file content (3 lines)");
    }

    // ── write_file ─────────────────────────────────────────────────────────

    #[test]
    fn outline_write_file_shows_path() {
        let entry = tool_entry("Wrote 120 bytes to src/main.rs", "call_1");
        let ctx = ctx_full("write_file", r#"{"path":"src/main.rs"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "write_file: src/main.rs");
    }

    #[test]
    fn outline_write_file_without_args() {
        let entry = tool_entry("ok", "call_1");
        let ctx = ctx_named("write_file");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "write_file: ?");
    }

    // ── edit_file ──────────────────────────────────────────────────────────

    #[test]
    fn outline_edit_file_shows_path() {
        let entry = tool_entry("Applied 1 edit", "call_1");
        let ctx = ctx_full(
            "edit_file",
            r#"{"path":"src/lib.rs","edits":[{"old":"foo","new":"bar"}]}"#,
            None,
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "edit_file: src/lib.rs");
    }

    // ── list_dir ───────────────────────────────────────────────────────────

    #[test]
    fn outline_list_dir_shows_path_and_entry_count() {
        let entry = tool_entry("src/\nmain.rs\nlib.rs\nCargo.toml", "call_1");
        let ctx = ctx_full("list_dir", r#"{"path":"src"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "list_dir: src (4 entries)");
    }

    // ── run_command ────────────────────────────────────────────────────────

    #[test]
    fn outline_run_command_shows_command_and_status() {
        let entry = tool_entry("cargo check output\nFinished dev target", "call_1");
        let ctx = ctx_full("run_command", r#"{"command":"cargo check"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "run_command: \"cargo check\" — ok (38 chars)");
    }

    #[test]
    fn outline_run_command_failed() {
        let entry = tool_entry("error: could not compile", "call_1");
        let ctx = ctx_full("run_command", r#"{"command":"cargo build"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert!(outline.contains("failed"), "should indicate failure");
    }

    // ── cargo_check with diagnostics ───────────────────────────────────────

    #[test]
    fn outline_cargo_check_from_diagnostics() {
        let entry = tool_entry("Compiling...", "call_1");
        let diagnostics = vec![
            Diagnostic {
                message: "mismatched types".to_owned(),
                code: Some("E0308".to_owned()),
                level: DiagnosticLevel::Error,
                spans: vec![],
                children: vec![],
                rendered: None,
            },
            Diagnostic {
                message: "unused variable".to_owned(),
                code: Some("unused_variables".to_owned()),
                level: DiagnosticLevel::Warning,
                spans: vec![],
                children: vec![],
                rendered: None,
            },
        ];
        let ctx = ctx_full(
            "cargo_check",
            "{}",
            Some(ToolResultDetails::Diagnostics(diagnostics)),
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_check: 1 errors, 1 warnings");
    }

    #[test]
    fn outline_cargo_check_clean() {
        let entry = tool_entry("Finished", "call_1");
        let ctx = ctx_named("cargo_check");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_check: 0 errors, 0 warnings");
    }

    #[test]
    fn outline_cargo_check_inferred_from_text() {
        let entry = tool_entry(
            "error[E0308]: mismatched types\n  --> src/main.rs:5:10\nwarning: unused\n",
            "call_1",
        );
        let ctx = ctx_named("cargo_check");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_check: 1 errors, 1 warnings");
    }

    // ── cargo_clippy ──────────────────────────────────────────────────────

    #[test]
    fn outline_cargo_clippy_from_diagnostics() {
        let entry = tool_entry("Checking...", "call_1");
        let diagnostics = vec![Diagnostic {
            message: "clippy lint".to_owned(),
            code: Some("clippy::needless_return".to_owned()),
            level: DiagnosticLevel::Warning,
            spans: vec![],
            children: vec![],
            rendered: None,
        }];
        let ctx = ctx_full(
            "cargo_clippy",
            "{}",
            Some(ToolResultDetails::Diagnostics(diagnostics)),
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_clippy: 0 errors, 1 warnings");
    }

    // ── cargo_test ─────────────────────────────────────────────────────────

    #[test]
    fn outline_cargo_test_from_summary_line() {
        let entry = tool_entry(
            "running 3 tests\ntest foo ... ok\ntest bar ... ok\ntest baz ... FAILED\ntest result: FAILED. 2 passed; 1 failed; 0 ignored",
            "call_1",
        );
        let ctx = ctx_named("cargo_test");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_test: 2 passed, 1 failed");
    }

    #[test]
    fn outline_cargo_test_all_passed() {
        let entry = tool_entry(
            "running 5 tests\ntest result: ok. 5 passed; 0 failed",
            "call_1",
        );
        let ctx = ctx_named("cargo_test");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_test: 5 passed, 0 failed");
    }

    #[test]
    fn outline_cargo_test_inferred_from_content() {
        let entry = tool_entry(
            "running 2 tests\ntest a ... ok\ntest b ... FAILED\ntest result: FAILED. 1 passed; 1 failed",
            "call_1",
        );
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "test results: 1 passed, 1 failed");
    }

    #[test]
    fn outline_cargo_test_fallback_to_individual_lines() {
        let entry = tool_entry(
            "test_foo ... ok\ntest_bar ... FAILED\ntest_baz ... ok",
            "call_1",
        );
        let ctx = ctx_named("cargo_test");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_test: 2 passed, 1 failed");
    }

    // ── cargo_fix ─────────────────────────────────────────────────────────

    #[test]
    fn outline_cargo_fix_from_diagnostics() {
        let entry = tool_entry("Fixing...", "call_1");
        let diagnostics = vec![Diagnostic {
            message: "unused import".to_owned(),
            code: Some("unused_imports".to_owned()),
            level: DiagnosticLevel::Warning,
            spans: vec![crate::diagnostic::DiagnosticSpan {
                file_name: "src/lib.rs".to_owned(),
                line_start: 1,
                line_end: 1,
                column_start: 1,
                column_end: 14,
                is_primary: true,
                label: None,
                suggestion: Some(DiagnosticSuggestion {
                    replacement: String::new(),
                    applicability: SuggestionApplicability::MachineApplicable,
                }),
            }],
            children: vec![],
            rendered: None,
        }];
        let ctx = ctx_full(
            "cargo_fix",
            "{}",
            Some(ToolResultDetails::Diagnostics(diagnostics)),
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_fix: 1 suggestions applied");
    }

    #[test]
    fn outline_cargo_fix_no_suggestions() {
        let entry = tool_entry("Nothing to fix", "call_1");
        let ctx = ctx_named("cargo_fix");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "cargo_fix: 0 suggestions applied");
    }

    // ── rustc_explain ──────────────────────────────────────────────────────

    #[test]
    fn outline_rustc_explain_shows_code() {
        let entry = tool_entry("The error code E0308 means...", "call_1");
        let ctx = ctx_full("rustc_explain", r#"{"code":"E0308"}"#, None);
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "rustc_explain: E0308");
    }

    // ── crates_io_lookup ─────────────────────────────────────────────────────

    #[test]
    fn outline_crates_io_search() {
        let entry = tool_entry(
            "<crate name=\"serde\" downloads=\"100000000\">\n  description: A serialization framework\n</crate>\n\n<crate name=\"serde_json\" downloads=\"50000000\">\n  description: JSON support\n</crate>",
            "call_1",
        );
        let ctx = ctx_full(
            "crates_io_lookup",
            r#"{"operation":"search","query":"serde"}"#,
            None,
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "crates_io search: \"serde\" (2 results)");
    }

    #[test]
    fn outline_crates_io_info() {
        let entry = tool_entry(
            "<crate name=\"serde\" version=\"1.0.200\" downloads=\"100000000\">\n  description: A serialization framework\n</crate>",
            "call_1",
        );
        let ctx = ctx_full(
            "crates_io_lookup",
            r#"{"operation":"info","crate_name":"serde"}"#,
            None,
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "crates_io info: serde v1.0.200");
    }

    #[test]
    fn outline_crates_io_versions() {
        let entry = tool_entry(
            "<version num=\"1.0.0\" status=\"available\" />\n<version num=\"0.9.0\" status=\"yanked\" />\n<version num=\"0.8.0\" status=\"available\" />",
            "call_1",
        );
        let ctx = ctx_full(
            "crates_io_lookup",
            r#"{"operation":"versions","crate_name":"serde"}"#,
            None,
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "crates_io versions: serde (3 versions)");
    }

    #[test]
    fn outline_crates_io_deps() {
        let entry = tool_entry(
            "<dependency name=\"proc-macro2\" req=\"^1.0\" kind=\"normal\" />\n<dependency name=\"quote\" req=\"^1.0\" kind=\"normal\" />",
            "call_1",
        );
        let ctx = ctx_full(
            "crates_io_lookup",
            r#"{"operation":"deps","crate_name":"serde"}"#,
            None,
        );
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "crates_io deps: serde (2 dependencies)");
    }

    #[test]
    fn outline_crates_io_inferred_from_content() {
        let entry = tool_entry(
            "<crate name=\"tokio\" version=\"1.40.0\" downloads=\"500000\">\n  description: async runtime\n</crate>",
            "call_1",
        );
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "crates.io info: 1.40.0");
    }

    #[test]
    fn outline_crates_io_search_inferred_from_content() {
        let entry = tool_entry(
            "<crate name=\"serde\" downloads=\"100\">\n  description: serialisation\n</crate>\n\n<crate name=\"serde_json\" downloads=\"50\">\n</crate>\n\n<crate name=\"serde_derive\" downloads=\"30\">\n</crate>",
            "call_1",
        );
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "crates.io search (3 results)");
    }

    // ── assistant messages ─────────────────────────────────────────────────

    #[test]
    fn outline_assistant_with_tool_calls() {
        let entry = assistant_entry(vec![
            tool_call("call_1", "read_file", r#"{"path":"src/main.rs"}"#),
            tool_call("call_2", "run_command", r#"{"command":"cargo test"}"#),
        ]);
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert!(outline.contains("read_file("));
        assert!(outline.contains("run_command("));
        assert!(outline.starts_with("Requested:"));
    }

    #[test]
    fn outline_assistant_empty_tool_calls() {
        let entry = assistant_entry(vec![]);
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "");
    }

    #[test]
    fn outline_assistant_truncates_long_args() {
        let long_path = "x".repeat(100);
        let args = format!(r#"{{"path":"{long_path}"}}"#);
        let entry = assistant_entry(vec![tool_call("call_1", "read_file", &args)]);
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert!(
            outline.len() <= 204,
            "outline should be within 200 char budget"
        );
        assert!(outline.ends_with('…') || outline.len() < 200);
    }

    // ── user messages ──────────────────────────────────────────────────────

    #[test]
    fn outline_user_message_truncates() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text(
            "x".repeat(500),
        )));
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert!(outline.len() <= 204);
        assert!(outline.ends_with('…'));
    }

    #[test]
    fn outline_user_message_preserves_short() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text("hello world")));
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "hello world");
    }

    // ── non-message payloads ───────────────────────────────────────────────

    #[test]
    fn outline_non_message_returns_empty() {
        let entry = test_entry(EntryPayload::Label {
            target_id: EntryId::new(),
            label: None,
        });
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "");
    }

    // ── unknown tool fallback ──────────────────────────────────────────────

    #[test]
    fn outline_unknown_tool_falls_back_to_truncation() {
        let entry = tool_entry("some extension tool output here", "call_1");
        let ctx = ctx_named("custom_tool");
        let outline = generate_outline(&entry, &ctx);
        assert_eq!(outline, "some extension tool output here");
    }

    #[test]
    fn outline_unknown_tool_truncates_long_output() {
        let entry = tool_entry(&"x".repeat(500), "call_1");
        let ctx = ctx_named("custom_tool");
        let outline = generate_outline(&entry, &ctx);
        assert!(outline.len() <= 204);
        assert!(outline.ends_with('…'));
    }

    // ── inferred diagnostics from content ───────────────────────────────────

    #[test]
    fn outline_inferred_diagnostics_from_content() {
        let entry = tool_entry(
            "error[E0308]: mismatched types\n  --> src/main.rs:5\nwarning: unused variable",
            "call_1",
        );
        let outline = generate_outline(&entry, &OutlineContext::default());
        assert_eq!(outline, "compiler diagnostics: 1 errors, 1 warnings");
    }

    // ── summary budget ─────────────────────────────────────────────────────

    #[test]
    fn summary_is_shorter_than_outline() {
        let entry = tool_entry(
            "<context>\nline1\nline2\nline3\nline4\nline5\n<context:end>",
            "call_1",
        );
        let ctx = ctx_named("read_file");
        let outline = generate_outline(&entry, &ctx);
        let summary = generate_summary(&entry, &ctx);
        assert!(summary.len() <= outline.len());
    }

    // ── utf-8 safety ───────────────────────────────────────────────────────

    #[test]
    fn truncate_with_ellipsis_utf8_safe() {
        let text = "αβγδεζηθ"; // 8 chars, 16 bytes
        let result = truncate_with_ellipsis(text, 5);
        assert!(result.ends_with('…'));
        assert!(result.is_char_boundary(0));
    }

    #[test]
    fn truncate_with_ellipsis_no_truncation_needed() {
        let text = "short";
        let result = truncate_with_ellipsis(text, 100);
        assert_eq!(result, "short");
        assert!(!result.ends_with('…'));
    }

    // ── extract_json_string_arg ────────────────────────────────────────────

    #[test]
    fn extract_json_arg_found() {
        let result = extract_json_string_arg(Some(r#"{"path":"src/main.rs","offset":10}"#), "path");
        assert_eq!(result, "src/main.rs");
    }

    #[test]
    fn extract_json_arg_missing_key() {
        let result = extract_json_string_arg(Some(r#"{"offset":10}"#), "path");
        assert_eq!(result, "?");
    }

    #[test]
    fn extract_json_arg_none_args() {
        let result = extract_json_string_arg(None, "path");
        assert_eq!(result, "?");
    }

    #[test]
    fn extract_json_arg_invalid_json() {
        let result = extract_json_string_arg(Some("not json"), "path");
        assert_eq!(result, "?");
    }

    // ── infer_test_counts ──────────────────────────────────────────────────

    #[test]
    fn infer_test_counts_from_summary_line() {
        let text = "test result: FAILED. 8 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out";
        let (passed, failed) = infer_test_counts(text);
        assert_eq!(passed, 8);
        assert_eq!(failed, 2);
    }

    #[test]
    fn infer_test_counts_all_ok() {
        let text = "test result: ok. 15 passed; 0 failed; 0 ignored";
        let (passed, failed) = infer_test_counts(text);
        assert_eq!(passed, 15);
        assert_eq!(failed, 0);
    }

    #[test]
    fn infer_test_counts_no_summary_line() {
        let text = "test_foo ... ok\ntest_bar ... FAILED\ntest_baz ... ok";
        let (passed, failed) = infer_test_counts(text);
        assert_eq!(passed, 2);
        assert_eq!(failed, 1);
    }

    #[test]
    fn infer_test_counts_no_tests() {
        let text = "Compiling foo v0.1.0";
        let (passed, failed) = infer_test_counts(text);
        assert_eq!(passed, 0);
        assert_eq!(failed, 0);
    }

    // ── infer_diagnostic_counts ────────────────────────────────────────────

    #[test]
    fn infer_diagnostic_counts_from_text() {
        let text =
            "error[E0308]: mismatched types\nwarning[unused_variables]: x\nerror[E0599]: no method";
        let (errors, warnings) = infer_diagnostic_counts(text);
        assert_eq!(errors, 2);
        assert_eq!(warnings, 1);
    }

    #[test]
    fn infer_diagnostic_counts_empty() {
        let (errors, warnings) = infer_diagnostic_counts("all clean");
        assert_eq!(errors, 0);
        assert_eq!(warnings, 0);
    }

    // ── count_diagnostic_levels ─────────────────────────────────────────────

    #[test]
    fn count_diagnostic_levels_structured() {
        let diags = vec![
            Diagnostic {
                message: "err".into(),
                code: None,
                level: DiagnosticLevel::Error,
                spans: vec![],
                children: vec![],
                rendered: None,
            },
            Diagnostic {
                message: "warn".into(),
                code: None,
                level: DiagnosticLevel::Warning,
                spans: vec![],
                children: vec![],
                rendered: None,
            },
            Diagnostic {
                message: "note".into(),
                code: None,
                level: DiagnosticLevel::Note,
                spans: vec![],
                children: vec![],
                rendered: None,
            },
        ];
        let (errors, warnings) = count_diagnostic_levels(&diags);
        assert_eq!(errors, 1);
        assert_eq!(warnings, 1);
    }

    // ── summarise_tool_args ───────────────────────────────────────────────

    #[test]
    fn summarise_tool_args_object() {
        let summary = summarise_tool_args(r#"{"path":"src/main.rs","offset":10}"#);
        assert!(summary.contains("path=src/main.rs"));
    }

    #[test]
    fn summarise_tool_args_truncates_long_values() {
        let long = "x".repeat(100);
        let args = format!(r#"{{"data":"{long}"}}"#);
        let summary = summarise_tool_args(&args);
        assert!(summary.len() < 80);
    }

    #[test]
    fn summarise_tool_args_non_json() {
        let summary = summarise_tool_args("not json");
        assert_eq!(summary, "not json");
    }
}
