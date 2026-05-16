//! Raw cargo JSON types for NDJSON parsing.
//!
//! These types are private to the rust module and are only used for
//! deserialization of cargo `--message-format=json` output.

use serde::Deserialize;

/// A line of cargo `--message-format=json` NDJSON output.
#[derive(Deserialize)]
pub(super) struct CargoMessage {
    /// The reason field distinguishing message types (`compiler-message`, `compiler-artifact`, etc.).
    pub reason: String,
    /// The compiler diagnostic, present only for `compiler-message` lines.
    #[serde(default)]
    pub message: Option<RawDiagnostic>,
    /// The build target, used to filter workspace vs dependency diagnostics.
    #[serde(default)]
    pub target: Option<RawTarget>,
}

/// The raw `message` field inside a `compiler-message` cargo line.
#[derive(Deserialize)]
pub(super) struct RawDiagnostic {
    /// The diagnostic message text.
    pub message: String,
    /// The error code (e.g. `E0308`), if any.
    pub code: Option<RawCode>,
    /// Severity level as a string (`"error"`, `"warning"`, etc.).
    pub level: String,
    /// Source spans associated with this diagnostic.
    #[serde(default)]
    pub spans: Vec<RawSpan>,
    /// Child diagnostics (notes, help suggestions).
    #[serde(default)]
    pub children: Vec<RawDiagnostic>,
    /// The compiler's rendered human-readable text.
    pub rendered: Option<String>,
}

/// A raw error code object.
#[derive(Deserialize)]
pub(super) struct RawCode {
    /// The code string (e.g. `"E0308"`).
    pub code: String,
}

/// A raw source span from the compiler's JSON output.
#[derive(Deserialize)]
pub(super) struct RawSpan {
    /// File path as reported by the compiler.
    pub file_name: String,
    /// 1-based start line.
    pub line_start: usize,
    /// 1-based end line.
    pub line_end: usize,
    /// 1-based start column.
    pub column_start: usize,
    /// 1-based end column.
    pub column_end: usize,
    /// Whether this is the primary span.
    pub is_primary: bool,
    /// The compiler's label for this span.
    pub label: Option<String>,
    /// Suggested replacement text, if any.
    pub suggested_replacement: Option<String>,
    /// Applicability of the suggestion (e.g. `"MachineApplicable"`).
    pub suggestion_applicability: Option<String>,
}

/// A raw build target from cargo's JSON output.
#[derive(Deserialize)]
pub(super) struct RawTarget {
    /// The source path of the target's root file.
    #[serde(default)]
    pub src_path: String,
}
