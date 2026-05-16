//! Conversion functions from raw cargo JSON types to core diagnostic types.

use rho_core::diagnostic::{Diagnostic, DiagnosticSuggestion, SuggestionApplicability};
use rho_core::{DiagnosticLevel, DiagnosticSpan};

use super::types::{RawDiagnostic, RawSpan};

/// Parse a severity level string into [`DiagnosticLevel`].
pub(super) fn parse_level(s: &str) -> DiagnosticLevel {
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
pub(super) fn parse_applicability(s: &str) -> SuggestionApplicability {
    match s {
        "MachineApplicable" => SuggestionApplicability::MachineApplicable,
        "MaybeIncorrect" => SuggestionApplicability::MaybeIncorrect,
        "HasPlaceholders" => SuggestionApplicability::HasPlaceholders,
        _ => SuggestionApplicability::Unspecified,
    }
}

/// Convert a raw span to a [`DiagnosticSpan`].
pub(super) fn convert_span(raw: &RawSpan) -> DiagnosticSpan {
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
pub(super) fn convert_diagnostic(raw: &RawDiagnostic) -> Diagnostic {
    Diagnostic {
        message: raw.message.clone(),
        code: raw.code.as_ref().map(|c| c.code.clone()),
        level: parse_level(&raw.level),
        spans: raw.spans.iter().map(convert_span).collect(),
        children: raw.children.iter().map(convert_diagnostic).collect(),
        rendered: raw.rendered.clone(),
    }
}
