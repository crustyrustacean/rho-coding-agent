//! Structured compiler diagnostic types.
//!
//! These types represent Rust compiler diagnostics extracted from
//! `cargo check --message-format=json` output. They are part of the core
//! data model because [`ToolResultDetails::Diagnostics`] references them.
//!
//! [`ToolResultDetails::Diagnostics`]: crate::tool::ToolResultDetails::Diagnostics

use serde::{Deserialize, Serialize};

/// Severity level of a compiler diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticLevel {
    /// A compilation error.
    Error,
    /// A compiler warning.
    Warning,
    /// An informational note attached to another diagnostic.
    Note,
    /// A help suggestion attached to another diagnostic.
    Help,
    /// A failure-note emitted after fatal errors (e.g. "aborting due to N errors").
    FailureNote,
}

impl std::fmt::Display for DiagnosticLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => write!(f, "error"),
            Self::Warning => write!(f, "warning"),
            Self::Note => write!(f, "note"),
            Self::Help => write!(f, "help"),
            Self::FailureNote => write!(f, "failure-note"),
        }
    }
}

/// How confidently a suggestion can be applied without human review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestionApplicability {
    /// Can be applied automatically with high confidence.
    MachineApplicable,
    /// May have unintended side effects; needs human review.
    MaybeIncorrect,
    /// The suggestion has placeholders the user must fill in.
    HasPlaceholders,
    /// Applicability is unknown.
    Unspecified,
}

/// A machine-applicable replacement suggestion for a diagnostic span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticSuggestion {
    /// The replacement text to apply at the span.
    pub replacement: String,
    /// How confidently this suggestion can be applied automatically.
    pub applicability: SuggestionApplicability,
}

/// A source location within a diagnostic.
///
/// Represents a contiguous range in a single file, with optional label text
/// and an optional machine-applicable replacement suggestion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticSpan {
    /// The file path (relative to the workspace root, as reported by the compiler).
    pub file_name: String,
    /// 1-based start line.
    pub line_start: usize,
    /// 1-based end line.
    pub line_end: usize,
    /// 1-based start column.
    pub column_start: usize,
    /// 1-based end column.
    pub column_end: usize,
    /// Whether this is the primary span (the one with the main underline).
    pub is_primary: bool,
    /// The compiler's label for this span (e.g. "expected `i32`, found `&str`").
    pub label: Option<String>,
    /// A machine-applicable replacement, if available.
    pub suggestion: Option<DiagnosticSuggestion>,
}

/// A structured compiler diagnostic.
///
/// Extracted from `cargo check --message-format=json` output. Only workspace
/// diagnostics are retained; dependency noise is filtered out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The diagnostic message (e.g. "mismatched types").
    pub message: String,
    /// The error code, if any (e.g. `"E0308"`).
    pub code: Option<String>,
    /// Severity level.
    pub level: DiagnosticLevel,
    /// Source spans associated with this diagnostic.
    pub spans: Vec<DiagnosticSpan>,
    /// Child diagnostics (notes, help suggestions) attached to this diagnostic.
    pub children: Vec<Diagnostic>,
    /// The compiler's rendered human-readable text for this diagnostic.
    pub rendered: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_level_display() {
        assert_eq!(DiagnosticLevel::Error.to_string(), "error");
        assert_eq!(DiagnosticLevel::Warning.to_string(), "warning");
        assert_eq!(DiagnosticLevel::Note.to_string(), "note");
        assert_eq!(DiagnosticLevel::Help.to_string(), "help");
        assert_eq!(DiagnosticLevel::FailureNote.to_string(), "failure-note");
    }

    #[test]
    fn diagnostic_round_trips_serde() {
        let diag = Diagnostic {
            message: "mismatched types".to_owned(),
            code: Some("E0308".to_owned()),
            level: DiagnosticLevel::Error,
            spans: vec![DiagnosticSpan {
                file_name: "src/lib.rs".to_owned(),
                line_start: 2,
                line_end: 2,
                column_start: 18,
                column_end: 25,
                is_primary: true,
                label: Some("expected `i32`, found `&str`".to_owned()),
                suggestion: None,
            }],
            children: vec![],
            rendered: Some("error[E0308]: mismatched types\n".to_owned()),
        };

        let json = serde_json::to_string(&diag).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(diag, back);
    }

    #[test]
    fn diagnostic_with_suggestion_round_trips_serde() {
        let diag = Diagnostic {
            message: "unused import".to_owned(),
            code: Some("unused_imports".to_owned()),
            level: DiagnosticLevel::Warning,
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
        };

        let json = serde_json::to_string(&diag).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(diag, back);
    }

    #[test]
    fn diagnostic_level_round_trips_serde() {
        for level in [
            DiagnosticLevel::Error,
            DiagnosticLevel::Warning,
            DiagnosticLevel::Note,
            DiagnosticLevel::Help,
            DiagnosticLevel::FailureNote,
        ] {
            let json = serde_json::to_string(&level).unwrap();
            let back: DiagnosticLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, back);
        }
    }

    #[test]
    fn suggestion_applicability_round_trips_serde() {
        for applicability in [
            SuggestionApplicability::MachineApplicable,
            SuggestionApplicability::MaybeIncorrect,
            SuggestionApplicability::HasPlaceholders,
            SuggestionApplicability::Unspecified,
        ] {
            let json = serde_json::to_string(&applicability).unwrap();
            let back: SuggestionApplicability = serde_json::from_str(&json).unwrap();
            assert_eq!(applicability, back);
        }
    }

    #[test]
    fn diagnostic_with_nested_children_round_trips_serde() {
        let diag = Diagnostic {
            message: "mismatched types".to_owned(),
            code: Some("E0308".to_owned()),
            level: DiagnosticLevel::Error,
            spans: vec![],
            children: vec![Diagnostic {
                message: "expected due to this".to_owned(),
                code: None,
                level: DiagnosticLevel::Note,
                spans: vec![DiagnosticSpan {
                    file_name: "src/lib.rs".to_owned(),
                    line_start: 2,
                    line_end: 2,
                    column_start: 12,
                    column_end: 15,
                    is_primary: false,
                    label: Some("expected due to this".to_owned()),
                    suggestion: None,
                }],
                children: vec![],
                rendered: None,
            }],
            rendered: None,
        };

        let json = serde_json::to_string(&diag).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(diag, back);
    }
}
