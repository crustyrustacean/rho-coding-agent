//! Rust tooling: [`CargoCheck`] tool and NDJSON diagnostic parsing.
//!
//! The [`CargoCheck`] tool runs `cargo check --message-format=json` within the
//! project sandbox and parses the NDJSON output into structured [`Diagnostic`]
//! values. Dependency noise (diagnostics from crates outside the workspace) is
//! filtered out, leaving only the diagnostics the model can act on.
//!
//! ## Diagnostic types
//!
//! The core diagnostic types ([`Diagnostic`], [`DiagnosticSpan`],
//! [`DiagnosticSuggestion`], [`DiagnosticLevel`]) live in `rho-core` and are
//! re-exported here for convenience. This module adds:
//!
//! - [`parse_cargo_diagnostics`] — NDJSON parser for cargo JSON output
//! - [`CargoCheck`] — the tool implementation
//!
//! [`Diagnostic`]: rho_core::Diagnostic
//! [`DiagnosticSpan`]: rho_core::DiagnosticSpan
//! [`DiagnosticSuggestion`]: rho_core::DiagnosticSuggestion
//! [`DiagnosticLevel`]: rho_core::DiagnosticLevel

use async_trait::async_trait;
use rho_core::{
    Diagnostic, DiagnosticLevel, DiagnosticSpan, DiagnosticSuggestion, Result, SandboxRoot,
    ShellExecutor, SuggestionApplicability, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult, ToolResultDetails},
};
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::Path;

// Re-export core diagnostic types for downstream convenience.
pub use rho_core::diagnostic::{
    Diagnostic as RustDiagnostic, DiagnosticLevel as RustDiagnosticLevel,
    DiagnosticSpan as RustDiagnosticSpan, DiagnosticSuggestion as RustDiagnosticSuggestion,
    SuggestionApplicability as RustSuggestionApplicability,
};

// ── Raw cargo JSON types (private, for deserialization only) ──────────────────

/// A line of cargo `--message-format=json` NDJSON output.
#[derive(Deserialize)]
struct CargoMessage {
    /// The reason field distinguishing message types (`compiler-message`, `compiler-artifact`, etc.).
    reason: String,
    /// The compiler diagnostic, present only for `compiler-message` lines.
    #[serde(default)]
    message: Option<RawDiagnostic>,
    /// The build target, used to filter workspace vs dependency diagnostics.
    #[serde(default)]
    target: Option<RawTarget>,
}

/// The raw `message` field inside a `compiler-message` cargo line.
#[derive(Deserialize)]
struct RawDiagnostic {
    /// The diagnostic message text.
    message: String,
    /// The error code (e.g. `E0308`), if any.
    code: Option<RawCode>,
    /// Severity level as a string (`"error"`, `"warning"`, etc.).
    level: String,
    /// Source spans associated with this diagnostic.
    #[serde(default)]
    spans: Vec<RawSpan>,
    /// Child diagnostics (notes, help suggestions).
    #[serde(default)]
    children: Vec<RawDiagnostic>,
    /// The compiler's rendered human-readable text.
    rendered: Option<String>,
}

/// A raw error code object.
#[derive(Deserialize)]
struct RawCode {
    /// The code string (e.g. `"E0308"`).
    code: String,
}

/// A raw source span from the compiler's JSON output.
#[derive(Deserialize)]
struct RawSpan {
    /// File path as reported by the compiler.
    file_name: String,
    /// 1-based start line.
    line_start: usize,
    /// 1-based end line.
    line_end: usize,
    /// 1-based start column.
    column_start: usize,
    /// 1-based end column.
    column_end: usize,
    /// Whether this is the primary span.
    is_primary: bool,
    /// The compiler's label for this span.
    label: Option<String>,
    /// Suggested replacement text, if any.
    suggested_replacement: Option<String>,
    /// Applicability of the suggestion (e.g. `"MachineApplicable"`).
    suggestion_applicability: Option<String>,
}

/// A raw build target from cargo's JSON output.
#[derive(Deserialize)]
struct RawTarget {
    /// The source path of the target's root file.
    #[serde(default)]
    src_path: String,
}

// ── Conversion from raw types ─────────────────────────────────────────────────

/// Parse a severity level string into [`DiagnosticLevel`].
fn parse_level(s: &str) -> DiagnosticLevel {
    match s {
        "error" => DiagnosticLevel::Error,
        "warning" => DiagnosticLevel::Warning,
        "help" => DiagnosticLevel::Help,
        "failure-note" => DiagnosticLevel::FailureNote,
        // "note" and anything unrecognised both map to Note.
        _ => DiagnosticLevel::Note,
    }
}

/// Parse a suggestion applicability string into [`SuggestionApplicability`].
fn parse_applicability(s: &str) -> SuggestionApplicability {
    match s {
        "MachineApplicable" => SuggestionApplicability::MachineApplicable,
        "MaybeIncorrect" => SuggestionApplicability::MaybeIncorrect,
        "HasPlaceholders" => SuggestionApplicability::HasPlaceholders,
        _ => SuggestionApplicability::Unspecified,
    }
}

/// Convert a raw span to a [`DiagnosticSpan`].
fn convert_span(raw: &RawSpan) -> DiagnosticSpan {
    let suggestion = raw
        .suggested_replacement
        .as_ref()
        .map(|replacement| DiagnosticSuggestion {
            replacement: replacement.clone(),
            applicability: raw
                .suggestion_applicability
                .as_deref()
                .map_or(SuggestionApplicability::Unspecified, parse_applicability),
        });

    DiagnosticSpan {
        file_name: raw.file_name.clone(),
        line_start: raw.line_start,
        line_end: raw.line_end,
        column_start: raw.column_start,
        column_end: raw.column_end,
        is_primary: raw.is_primary,
        label: raw.label.clone(),
        suggestion,
    }
}

/// Convert a raw diagnostic to a [`Diagnostic`].
fn convert_diagnostic(raw: &RawDiagnostic) -> Diagnostic {
    Diagnostic {
        message: raw.message.clone(),
        code: raw.code.as_ref().map(|c| c.code.clone()),
        level: parse_level(&raw.level),
        spans: raw.spans.iter().map(convert_span).collect(),
        children: raw.children.iter().map(convert_diagnostic).collect(),
        rendered: raw.rendered.clone(),
    }
}

// ── NDJSON parsing ────────────────────────────────────────────────────────────

/// Parse cargo `--message-format=json` NDJSON output into structured diagnostics.
///
/// Filters to only `compiler-message` lines whose target source path lies
/// within `workspace_root`. This removes dependency noise — diagnostics from
/// crates in `~/.cargo/registry` or vendored paths outside the workspace.
///
/// Lines that fail to parse as JSON are silently skipped (cargo sometimes
/// emits non-JSON progress lines to stdout).
pub fn parse_cargo_diagnostics(ndjson: &str, workspace_root: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for line in ndjson.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else {
            continue;
        };

        if msg.reason != "compiler-message" {
            continue;
        }

        let Some(raw_diag) = &msg.message else {
            continue;
        };

        // Filter: skip diagnostics from outside the workspace.
        if let Some(target) = &msg.target
            && !target.src_path.is_empty()
            && !path_is_within(workspace_root, &target.src_path)
        {
            continue;
        }

        // Skip summary diagnostics with no actionable content.
        if raw_diag.level == "failure-note"
            || (raw_diag.code.is_none()
                && raw_diag.spans.is_empty()
                && raw_diag.message.starts_with("aborting due to"))
        {
            continue;
        }

        diagnostics.push(convert_diagnostic(raw_diag));
    }

    diagnostics
}

/// Check if `candidate` path is within `root`.
///
/// Uses simple string prefix matching after normalizing separators.
/// This avoids canonicalization (which requires the path to exist on disk).
fn path_is_within(root: &Path, candidate: &str) -> bool {
    let root_str = root.to_string_lossy().replace('\\', "/");
    let candidate_normalized = candidate.replace('\\', "/");
    candidate_normalized.starts_with(&root_str)
}

// ── Diagnostic formatting ─────────────────────────────────────────────────────

/// Format diagnostics into a human-readable summary for the model.
///
/// Uses the compiler's `rendered` text when available, falling back to a
/// structured format. Includes machine-applicable suggestions inline.
/// `tool_label` is used in the header (e.g. `"cargo check"` or `"cargo clippy"`).
fn format_diagnostics_for_model(tool_label: &str, diagnostics: &[Diagnostic]) -> String {
    if diagnostics.is_empty() {
        return format!("{tool_label}: no errors or warnings");
    }

    let error_count = diagnostics
        .iter()
        .filter(|d| d.level == DiagnosticLevel::Error)
        .count();
    let warning_count = diagnostics
        .iter()
        .filter(|d| d.level == DiagnosticLevel::Warning)
        .count();

    let mut output = String::new();
    let _ = write!(
        output,
        "{tool_label}: {error_count} error(s), {warning_count} warning(s)\n\n"
    );

    for diag in diagnostics {
        if let Some(rendered) = &diag.rendered {
            output.push_str(rendered.trim());
            output.push('\n');
        } else {
            // Fallback: structured format.
            let _ = write!(output, "{}: {}", diag.level, diag.message);
            if let Some(code) = &diag.code {
                let _ = write!(output, " [{code}]");
            }
            output.push('\n');
            for span in &diag.spans {
                let _ = writeln!(
                    output,
                    "  --> {}:{}:{}",
                    span.file_name, span.line_start, span.column_start
                );
                if let Some(label) = &span.label {
                    let _ = writeln!(output, "      {label}");
                }
            }
        }

        // Surface machine-applicable suggestions prominently.
        append_machine_applicable_suggestions(&mut output, diag);

        output.push('\n');
    }

    output.trim_end().to_owned()
}

/// Append `[machine-applicable fix]` lines for suggestions in a diagnostic's
/// children and top-level spans.
fn append_machine_applicable_suggestions(output: &mut String, diag: &Diagnostic) {
    for child in &diag.children {
        for span in &child.spans {
            if let Some(suggestion) = &span.suggestion
                && suggestion.applicability == SuggestionApplicability::MachineApplicable
            {
                let _ = writeln!(
                    output,
                    "  [machine-applicable fix] at {}:{}:{} → replace with: {}",
                    span.file_name, span.line_start, span.column_start, suggestion.replacement,
                );
            }
        }
    }

    for span in &diag.spans {
        if let Some(suggestion) = &span.suggestion
            && suggestion.applicability == SuggestionApplicability::MachineApplicable
        {
            let _ = writeln!(
                output,
                "  [machine-applicable fix] at {}:{}:{} → replace with: {}",
                span.file_name, span.line_start, span.column_start, suggestion.replacement,
            );
        }
    }
}

// ── CargoCheck tool ───────────────────────────────────────────────────────────

/// Run `cargo check --message-format=json` and return structured diagnostics.
///
/// The tool:
/// 1. Runs `cargo check --message-format=json` via the configured
///    [`ShellExecutor`] within the sandbox root.
/// 2. Parses the NDJSON output into [`Diagnostic`] values.
/// 3. Filters out dependency noise (diagnostics from outside the workspace).
/// 4. Returns a human-readable summary for the model, with structured
///    [`Diagnostic`] data in [`ToolResultDetails::Diagnostics`].
///
/// ## Optional parameters
///
/// - `package` — restrict check to a single workspace member (passed as
///   `--package <name>`). Omit to check the entire workspace.
pub struct CargoCheck {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoCheck {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_check")
    }

    fn description(&self) -> &str {
        "Run `cargo check` and return structured compiler diagnostics. \
         Returns error codes, messages, file locations, and machine-applicable \
         fix suggestions. Use this before attempting to fix compilation errors."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: check only this workspace member package name."
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
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo check --message-format=json");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel)
            .await?;

        execute_cargo_diagnostic_tool("cargo check", &shell_output.stdout, self.root.path())
    }
}

// ── CargoClippy tool ──────────────────────────────────────────────────────────

/// Run `cargo clippy --message-format=json` and return structured diagnostics.
///
/// Same as [`CargoCheck`] but runs Clippy instead of the basic compiler check.
/// Clippy diagnostics include lint names and additional code-quality warnings
/// beyond what `cargo check` reports.
///
/// ## Optional parameters
///
/// - `package` — restrict to a single workspace member.
pub struct CargoClippy {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoClippy {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_clippy")
    }

    fn description(&self) -> &str {
        "Run `cargo clippy` and return structured lint diagnostics. \
         Returns lint names, messages, file locations, and machine-applicable \
         fix suggestions. Use this to find code-quality issues beyond \
         compilation errors."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: check only this workspace member package name."
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
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo clippy --message-format=json");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel)
            .await?;

        execute_cargo_diagnostic_tool("cargo clippy", &shell_output.stdout, self.root.path())
    }
}

// ── RustcExplain tool ─────────────────────────────────────────────────────────

/// Run `rustc --explain <code>` and return the explanation text.
///
/// The model can use this to understand what an error code means before
/// attempting a fix. The explanation text is returned verbatim from `rustc`.
pub struct RustcExplain {
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for RustcExplain {
    fn name(&self) -> ToolName {
        ToolName::from("rustc_explain")
    }

    fn description(&self) -> &str {
        "Look up the explanation for a Rust compiler error code (e.g. E0308). \
         Returns the detailed explanation text from `rustc --explain`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "The error code to explain (e.g. \"E0308\")."
                }
            },
            "required": ["code"]
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
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let code = arguments
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                rho_core::RhoError::Unexpected(anyhow::anyhow!(
                    "rustc_explain: missing required argument `code`"
                ))
            })?;

        // Validate the code looks like an error code (E followed by digits).
        if !is_valid_error_code(code) {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "invalid error code: {code:?} — expected format like \"E0308\""
            ))));
        }

        let cmd = format!("rustc --explain {code}");

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel)
            .await?;

        if shell_output.is_success() && !shell_output.stdout.trim().is_empty() {
            Ok(ToolOutcome::Immediate(ToolResult::success(
                shell_output.stdout.trim(),
            )))
        } else {
            // rustc --explain exits non-zero for unknown codes.
            let msg = if shell_output.stderr.trim().is_empty() {
                format!("no explanation found for error code {code}")
            } else {
                shell_output.stderr.trim().to_owned()
            };
            Ok(ToolOutcome::Immediate(ToolResult::error(msg)))
        }
    }
}

/// Validate that an error code looks like `E0308` (E followed by 1–4 digits).
fn is_valid_error_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.is_empty() || bytes[0] != b'E' {
        return false;
    }
    let digits = &bytes[1..];
    !digits.is_empty() && digits.len() <= 4 && digits.iter().all(u8::is_ascii_digit)
}

// ── Shared execution helper ───────────────────────────────────────────────────

/// Shared logic for `CargoCheck` and `CargoClippy`: parse NDJSON, format, and
/// build the `ToolOutcome`.
///
/// Returns `Result` for ergonomic use with the `?` operator at call sites,
/// even though the function itself is infallible.
#[allow(clippy::unnecessary_wraps)]
fn execute_cargo_diagnostic_tool(
    tool_label: &str,
    stdout: &str,
    workspace_root: &Path,
) -> Result<ToolOutcome> {
    let diagnostics = parse_cargo_diagnostics(stdout, workspace_root);

    let summary = format_diagnostics_for_model(tool_label, &diagnostics);
    let is_error = diagnostics
        .iter()
        .any(|d| d.level == DiagnosticLevel::Error);

    let details = ToolResultDetails::Diagnostics(diagnostics);

    Ok(ToolOutcome::Immediate(ToolResult {
        output: summary,
        is_error,
        details,
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Diagnostic level parsing ──────────────────────────────────────────

    #[test]
    fn parses_level_error() {
        assert_eq!(parse_level("error"), DiagnosticLevel::Error);
    }

    #[test]
    fn parses_level_warning() {
        assert_eq!(parse_level("warning"), DiagnosticLevel::Warning);
    }

    #[test]
    fn parses_level_note() {
        assert_eq!(parse_level("note"), DiagnosticLevel::Note);
    }

    #[test]
    fn parses_level_help() {
        assert_eq!(parse_level("help"), DiagnosticLevel::Help);
    }

    #[test]
    fn parses_level_failure_note() {
        assert_eq!(parse_level("failure-note"), DiagnosticLevel::FailureNote);
    }

    #[test]
    fn parses_level_unknown_defaults_to_note() {
        assert_eq!(parse_level("ice"), DiagnosticLevel::Note);
    }

    // ── Suggestion applicability parsing ──────────────────────────────────

    #[test]
    fn parses_applicability_machine_applicable() {
        assert_eq!(
            parse_applicability("MachineApplicable"),
            SuggestionApplicability::MachineApplicable
        );
    }

    #[test]
    fn parses_applicability_maybe_incorrect() {
        assert_eq!(
            parse_applicability("MaybeIncorrect"),
            SuggestionApplicability::MaybeIncorrect
        );
    }

    #[test]
    fn parses_applicability_has_placeholders() {
        assert_eq!(
            parse_applicability("HasPlaceholders"),
            SuggestionApplicability::HasPlaceholders
        );
    }

    #[test]
    fn parses_applicability_unknown_defaults_to_unspecified() {
        assert_eq!(
            parse_applicability("SomethingElse"),
            SuggestionApplicability::Unspecified
        );
    }

    // ── path_is_within ────────────────────────────────────────────────────

    #[test]
    fn path_within_workspace_returns_true() {
        let root = Path::new("C:/Users/dev/project");
        assert!(path_is_within(root, "C:/Users/dev/project/src/main.rs"));
    }

    #[test]
    fn path_outside_workspace_returns_false() {
        let root = Path::new("C:/Users/dev/project");
        assert!(!path_is_within(
            root,
            "C:/Users/.cargo/registry/src/crate/lib.rs"
        ));
    }

    #[test]
    fn path_within_normalizes_backslashes() {
        let root = Path::new("C:/Users/dev/project");
        assert!(path_is_within(
            root,
            "C:\\Users\\dev\\project\\src\\main.rs"
        ));
    }

    // ── NDJSON parsing ────────────────────────────────────────────────────

    /// Build a minimal `compiler-message` NDJSON line for testing.
    fn make_compiler_message(
        src_path: &str,
        level: &str,
        message: &str,
        code: Option<&str>,
    ) -> String {
        let code_json = match code {
            Some(c) => format!(r#"{{"code":"{c}"}}"#),
            None => "null".to_owned(),
        };
        format!(
            r#"{{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"{src_path}","edition":"2021","doc":true,"doctest":true,"test":true}},"message":{{"message":"{message}","code":{code_json},"level":"{level}","spans":[{{"file_name":"src/lib.rs","byte_start":0,"byte_end":10,"line_start":1,"line_end":1,"column_start":1,"column_end":11,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}],"children":[],"rendered":"{level}: {message}\\n"}}}}"#
        )
    }

    #[test]
    fn parses_compiler_message_with_error() {
        let ndjson = r#"{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"message":"mismatched types","code":{"code":"E0308","explanation":null},"level":"error","spans":[{"file_name":"src/lib.rs","byte_start":29,"byte_end":36,"line_start":2,"line_end":2,"column_start":18,"column_end":25,"is_primary":true,"text":[{"text":"    let x: i32 = \"hello\";","highlight_start":18,"highlight_end":25}],"label":"expected `i32`, found `&str`","suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":"error[E0308]: mismatched types\n"}}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "mismatched types");
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0308"));
        assert_eq!(diagnostics[0].level, DiagnosticLevel::Error);
        assert_eq!(diagnostics[0].spans.len(), 1);
        assert_eq!(diagnostics[0].spans[0].file_name, "src/lib.rs");
        assert_eq!(diagnostics[0].spans[0].line_start, 2);
        assert_eq!(diagnostics[0].spans[0].column_start, 18);
        assert!(diagnostics[0].spans[0].is_primary);
        assert_eq!(
            diagnostics[0].spans[0].label.as_deref(),
            Some("expected `i32`, found `&str`")
        );
    }

    #[test]
    fn filters_out_dependency_diagnostics() {
        let workspace_msg = make_compiler_message(
            "C:/project/src/lib.rs",
            "warning",
            "unused variable",
            Some("unused_variables"),
        );
        let dep_msg = make_compiler_message(
            "C:/Users/.cargo/registry/src/somecrate-1.0.0/src/lib.rs",
            "warning",
            "dep warning",
            None,
        );

        let ndjson = format!("{workspace_msg}\n{dep_msg}");
        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(&ndjson, root);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "unused variable");
    }

    #[test]
    fn filters_out_aborting_summary() {
        let ndjson = r#"{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"message":"aborting due to 2 previous errors","code":null,"level":"error","spans":[],"children":[],"rendered":"error: aborting due to 2 previous errors\n"}}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn filters_out_failure_notes() {
        let ndjson = r#"{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"message":"For more information about an error, try `rustc --explain E0308`.","code":null,"level":"failure-note","spans":[],"children":[],"rendered":"For more information...\n"}}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_non_json_lines() {
        let ndjson = format!(
            "   Compiling mylib v0.1.0\n{}",
            make_compiler_message(
                "C:/project/src/lib.rs",
                "warning",
                "unused import",
                Some("unused_imports")
            )
        );

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(&ndjson, root);
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn skips_artifact_messages() {
        let ndjson = r#"{"reason":"compiler-artifact","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"profile":{"opt_level":"0","debuginfo":2,"debug_assertions":true,"overflow_checks":true,"test":false},"features":[],"filenames":[],"executable":null,"fresh":false}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn skips_build_finished_messages() {
        let ndjson = r#"{"reason":"build-finished","success":true}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn parses_suggestion_with_machine_applicable() {
        let ndjson = r#"{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"message":"unused import: `std::io`","code":{"code":"unused_imports"},"level":"warning","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":12,"line_start":1,"line_end":1,"column_start":1,"column_end":13,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[{"message":"remove the whole `use` item","code":null,"level":"help","spans":[{"file_name":"src/lib.rs","byte_start":0,"byte_end":13,"line_start":1,"line_end":1,"column_start":1,"column_end":14,"is_primary":true,"text":[],"label":null,"suggested_replacement":"","suggestion_applicability":"MachineApplicable","expansion":null}],"children":[],"rendered":null}],"rendered":"warning: unused import\n"}}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].children.len(), 1);

        let child = &diagnostics[0].children[0];
        assert_eq!(child.level, DiagnosticLevel::Help);
        assert_eq!(child.spans.len(), 1);

        let suggestion = child.spans[0].suggestion.as_ref().unwrap();
        assert_eq!(suggestion.replacement, "");
        assert_eq!(
            suggestion.applicability,
            SuggestionApplicability::MachineApplicable
        );
    }

    #[test]
    fn parses_diagnostic_with_children() {
        let ndjson = r#"{"reason":"compiler-message","package_id":"path+file:///C:/project#0.1.0","manifest_path":"C:\\project\\Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"C:/project/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true},"message":{"message":"mismatched types","code":{"code":"E0308"},"level":"error","spans":[{"file_name":"src/lib.rs","byte_start":29,"byte_end":36,"line_start":2,"line_end":2,"column_start":18,"column_end":25,"is_primary":true,"text":[],"label":"expected i32","suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[{"message":"expected due to this","code":null,"level":"note","spans":[{"file_name":"src/lib.rs","byte_start":23,"byte_end":26,"line_start":2,"line_end":2,"column_start":12,"column_end":15,"is_primary":false,"text":[],"label":"expected due to this","suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":null}],"rendered":"error[E0308]: mismatched types\n"}}"#;

        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(ndjson, root);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].children.len(), 1);
        assert_eq!(diagnostics[0].children[0].message, "expected due to this");
        assert_eq!(diagnostics[0].children[0].level, DiagnosticLevel::Note);
    }

    #[test]
    fn empty_input_returns_empty() {
        let diagnostics = parse_cargo_diagnostics("", Path::new("C:/project"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn blank_lines_are_skipped() {
        let diagnostics = parse_cargo_diagnostics("\n\n  \n", Path::new("C:/project"));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn multiple_diagnostics_from_same_crate() {
        let msg1 = make_compiler_message(
            "C:/project/src/lib.rs",
            "error",
            "type mismatch",
            Some("E0308"),
        );
        let msg2 = make_compiler_message(
            "C:/project/src/lib.rs",
            "warning",
            "unused variable",
            Some("unused_variables"),
        );

        let ndjson = format!("{msg1}\n{msg2}");
        let root = Path::new("C:/project");
        let diagnostics = parse_cargo_diagnostics(&ndjson, root);

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].level, DiagnosticLevel::Error);
        assert_eq!(diagnostics[1].level, DiagnosticLevel::Warning);
    }

    // ── Formatting ────────────────────────────────────────────────────────

    #[test]
    fn format_empty_diagnostics_shows_no_errors() {
        let output = format_diagnostics_for_model("cargo check", &[]);
        assert_eq!(output, "cargo check: no errors or warnings");
    }

    #[test]
    fn format_uses_rendered_when_available() {
        let diag = Diagnostic {
            message: "mismatched types".to_owned(),
            code: Some("E0308".to_owned()),
            level: DiagnosticLevel::Error,
            spans: vec![],
            children: vec![],
            rendered: Some("error[E0308]: mismatched types\n --> src/lib.rs:2:18".to_owned()),
        };
        let output = format_diagnostics_for_model("cargo check", &[diag]);
        assert!(output.contains("error[E0308]: mismatched types"));
        assert!(output.contains("1 error(s), 0 warning(s)"));
    }

    #[test]
    fn format_falls_back_to_structured_when_no_rendered() {
        let diag = Diagnostic {
            message: "unused variable".to_owned(),
            code: Some("unused_variables".to_owned()),
            level: DiagnosticLevel::Warning,
            spans: vec![DiagnosticSpan {
                file_name: "src/main.rs".to_owned(),
                line_start: 5,
                line_end: 5,
                column_start: 9,
                column_end: 10,
                is_primary: true,
                label: Some("help: prefix with _".to_owned()),
                suggestion: None,
            }],
            children: vec![],
            rendered: None,
        };
        let output = format_diagnostics_for_model("cargo check", &[diag]);
        assert!(output.contains("warning: unused variable [unused_variables]"));
        assert!(output.contains("--> src/main.rs:5:9"));
    }

    #[test]
    fn format_surfaces_machine_applicable_suggestions_from_children() {
        let diag = Diagnostic {
            message: "unused import".to_owned(),
            code: Some("unused_imports".to_owned()),
            level: DiagnosticLevel::Warning,
            spans: vec![],
            children: vec![Diagnostic {
                message: "remove the whole `use` item".to_owned(),
                code: None,
                level: DiagnosticLevel::Help,
                spans: vec![DiagnosticSpan {
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
            }],
            rendered: Some("warning: unused import\n".to_owned()),
        };
        let output = format_diagnostics_for_model("cargo check", &[diag]);
        assert!(output.contains("[machine-applicable fix]"));
    }

    #[test]
    fn format_counts_errors_and_warnings_separately() {
        let diagnostics = vec![
            Diagnostic {
                message: "error one".to_owned(),
                code: None,
                level: DiagnosticLevel::Error,
                spans: vec![],
                children: vec![],
                rendered: Some("error: error one\n".to_owned()),
            },
            Diagnostic {
                message: "warning one".to_owned(),
                code: None,
                level: DiagnosticLevel::Warning,
                spans: vec![],
                children: vec![],
                rendered: Some("warning: warning one\n".to_owned()),
            },
            Diagnostic {
                message: "error two".to_owned(),
                code: None,
                level: DiagnosticLevel::Error,
                spans: vec![],
                children: vec![],
                rendered: Some("error: error two\n".to_owned()),
            },
        ];
        let output = format_diagnostics_for_model("cargo check", &diagnostics);
        assert!(output.contains("2 error(s), 1 warning(s)"));
    }

    // ── CargoCheck tool via MockShellExecutor ─────────────────────────────

    #[tokio::test]
    async fn cargo_check_tool_returns_success_on_clean_build() {
        let env = rho_test_helpers::FileTestEnv::new();
        let ndjson = r#"{"reason":"build-finished","success":true}"#;

        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            ndjson.to_owned(),
            String::new(),
            0,
        )]);

        let tool = CargoCheck {
            root: env.sandbox().clone(),
            executor: Box::new(mock.clone()),
        };

        let cancel = CancellationToken::new();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(!r.is_error);
                assert!(r.output.contains("no errors or warnings"));
                assert_eq!(r.details, ToolResultDetails::Diagnostics(vec![]));
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }

        let commands = mock.commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("cargo check --message-format=json"));
    }

    #[tokio::test]
    async fn cargo_check_tool_returns_error_on_compilation_failure() {
        let env = rho_test_helpers::FileTestEnv::new();
        let root_path = env.root().to_string_lossy().replace('\\', "/");

        let ndjson = format!(
            r#"{{"reason":"compiler-message","package_id":"test","manifest_path":"test","target":{{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"{root_path}/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true}},"message":{{"message":"mismatched types","code":{{"code":"E0308"}},"level":"error","spans":[{{"file_name":"src/lib.rs","byte_start":0,"byte_end":10,"line_start":1,"line_end":1,"column_start":1,"column_end":11,"is_primary":true,"text":[],"label":"expected i32","suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}],"children":[],"rendered":"error[E0308]: mismatched types\n"}}}}"#
        );

        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            ndjson,
            String::new(),
            101,
        )]);

        let tool = CargoCheck {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
                assert!(r.output.contains("1 error(s)"));
                assert!(r.output.contains("mismatched types"));
                match &r.details {
                    ToolResultDetails::Diagnostics(diags) => {
                        assert_eq!(diags.len(), 1);
                        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
                    }
                    _ => panic!("expected Diagnostics details"),
                }
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    #[tokio::test]
    async fn cargo_check_tool_passes_package_argument() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
            String::new(),
            0,
        )]);

        let tool = CargoCheck {
            root: env.sandbox().clone(),
            executor: Box::new(mock.clone()),
        };

        let cancel = CancellationToken::new();
        let _result = tool
            .execute(serde_json::json!({"package": "rho-core"}), cancel)
            .await
            .unwrap();

        let commands = mock.commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("--package rho-core"));
    }

    #[tokio::test]
    async fn cargo_check_tool_returns_cancelled_when_cancelled() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![]);

        let tool = CargoCheck {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
                assert_eq!(r.output, "cancelled");
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    // ── CargoClippy tool via MockShellExecutor ─────────────────────

    #[tokio::test]
    async fn cargo_clippy_tool_returns_success_on_clean_build() {
        let env = rho_test_helpers::FileTestEnv::new();
        let ndjson = r#"{"reason":"build-finished","success":true}"#;

        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            ndjson.to_owned(),
            String::new(),
            0,
        )]);

        let tool = CargoClippy {
            root: env.sandbox().clone(),
            executor: Box::new(mock.clone()),
        };

        let cancel = CancellationToken::new();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(!r.is_error);
                assert!(r.output.contains("cargo clippy: no errors or warnings"));
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }

        let commands = mock.commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("cargo clippy --message-format=json"));
    }

    #[tokio::test]
    async fn cargo_clippy_tool_passes_package_argument() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
            String::new(),
            0,
        )]);

        let tool = CargoClippy {
            root: env.sandbox().clone(),
            executor: Box::new(mock.clone()),
        };

        let cancel = CancellationToken::new();
        let _result = tool
            .execute(serde_json::json!({"package": "rho-core"}), cancel)
            .await
            .unwrap();

        let commands = mock.commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("cargo clippy --message-format=json --package rho-core"));
    }

    #[tokio::test]
    async fn cargo_clippy_tool_returns_cancelled_when_cancelled() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![]);

        let tool = CargoClippy {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
                assert_eq!(r.output, "cancelled");
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    // ── RustcExplain tool tests ─────────────────────────────────

    #[test]
    fn valid_error_code_e0308() {
        assert!(is_valid_error_code("E0308"));
    }

    #[test]
    fn valid_error_code_e0001() {
        assert!(is_valid_error_code("E0001"));
    }

    #[test]
    fn invalid_error_code_no_e_prefix() {
        assert!(!is_valid_error_code("0308"));
    }

    #[test]
    fn invalid_error_code_too_many_digits() {
        assert!(!is_valid_error_code("E00001"));
    }

    #[test]
    fn invalid_error_code_empty() {
        assert!(!is_valid_error_code(""));
    }

    #[test]
    fn invalid_error_code_letters_after_e() {
        assert!(!is_valid_error_code("Eabcd"));
    }

    #[test]
    fn invalid_error_code_just_e() {
        assert!(!is_valid_error_code("E"));
    }

    #[tokio::test]
    async fn rustc_explain_returns_explanation_on_success() {
        let env = rho_test_helpers::FileTestEnv::new();
        let explanation = "Expected type did not match the received type.\n";
        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            explanation.to_owned(),
            String::new(),
            0,
        )]);

        let tool = RustcExplain {
            root: env.sandbox().clone(),
            executor: Box::new(mock.clone()),
        };

        let cancel = CancellationToken::new();
        let result = tool
            .execute(serde_json::json!({"code": "E0308"}), cancel)
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(!r.is_error);
                assert!(r.output.contains("Expected type did not match"));
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }

        let commands = mock.commands();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains("rustc --explain E0308"));
    }

    #[tokio::test]
    async fn rustc_explain_returns_error_for_unknown_code() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            String::new(),
            "error: unknown error code\n".to_owned(),
            1,
        )]);

        let tool = RustcExplain {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        let result = tool
            .execute(serde_json::json!({"code": "E9999"}), cancel)
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    #[tokio::test]
    async fn rustc_explain_rejects_invalid_code_format() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![]);

        let tool = RustcExplain {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        let result = tool
            .execute(serde_json::json!({"code": "not-a-code"}), cancel)
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
                assert!(r.output.contains("invalid error code"));
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    #[tokio::test]
    async fn rustc_explain_returns_cancelled_when_cancelled() {
        let env = rho_test_helpers::FileTestEnv::new();
        let mock = rho_test_helpers::MockShellExecutor::new(vec![]);

        let tool = RustcExplain {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool
            .execute(serde_json::json!({"code": "E0308"}), cancel)
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(r.is_error);
                assert_eq!(r.output, "cancelled");
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }

    // ── Format label tests ─────────────────────────────────────

    #[test]
    fn format_clippy_label_in_header() {
        let output = format_diagnostics_for_model("cargo clippy", &[]);
        assert_eq!(output, "cargo clippy: no errors or warnings");
    }

    #[tokio::test]
    async fn cargo_clippy_tool_returns_warnings_for_lint() {
        let env = rho_test_helpers::FileTestEnv::new();
        let root_path = env.root().to_string_lossy().replace('\\', "/");

        let ndjson = format!(
            r#"{{"reason":"compiler-message","package_id":"test","manifest_path":"test","target":{{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"{root_path}/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true}},"message":{{"message":"redundant clone","code":{{"code":"clippy::redundant_clone"}},"level":"warning","spans":[{{"file_name":"src/lib.rs","byte_start":0,"byte_end":10,"line_start":1,"line_end":1,"column_start":1,"column_end":11,"is_primary":true,"text":[],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}],"children":[],"rendered":"warning: redundant clone\n"}}}}"#
        );

        let mock = rho_test_helpers::MockShellExecutor::new(vec![rho_core::ShellOutput::new(
            ndjson,
            String::new(),
            0,
        )]);

        let tool = CargoClippy {
            root: env.sandbox().clone(),
            executor: Box::new(mock),
        };

        let cancel = CancellationToken::new();
        let result = tool.execute(serde_json::json!({}), cancel).await.unwrap();

        match result {
            ToolOutcome::Immediate(r) => {
                assert!(!r.is_error);
                assert!(r.output.contains("cargo clippy:"));
                assert!(r.output.contains("0 error(s), 1 warning(s)"));
                match &r.details {
                    ToolResultDetails::Diagnostics(diags) => {
                        assert_eq!(diags.len(), 1);
                        assert_eq!(diags[0].code.as_deref(), Some("clippy::redundant_clone"));
                        assert_eq!(diags[0].level, DiagnosticLevel::Warning);
                    }
                    _ => panic!("expected Diagnostics details"),
                }
            }
            ToolOutcome::Streamed(_) => panic!("expected immediate result"),
        }
    }
}
