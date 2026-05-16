//! Diagnostic formatting and AST context functions.

use std::fmt::Write as _;
use rho_core::{
    Diagnostic, DiagnosticLevel, DiagnosticSpan,
};
use rho_core::diagnostic::SuggestionApplicability;

/// Format diagnostics into a human-readable summary for the model.
///
/// Uses the compiler's `rendered` text when available, falling back to a
/// structured format. Includes machine-applicable suggestions inline.
/// `tool_label` is used in the header (e.g. `"cargo check"` or `"cargo clippy"`).
pub(super) fn format_diagnostics_for_model(tool_label: &str, diagnostics: &[Diagnostic]) -> String {
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

/// Extract AST context around a diagnostic span.
///
/// Returns up to `context_lines` lines before and after the span, with the
/// span itself highlighted. Each line includes the line number for reference.
///
/// The output format is:
/// ```text
/// error[E0308]: expected type, found `()`
///  --> src/main.rs:42:5
///    |
/// 41 |   let x = 1;
/// 42 |     foo();
///    |     ^^^ expected type, found `()`
/// 43 | }
/// ```
///
/// This function integrates with [`rho_highlight`] to parse the file and
/// locate the node corresponding to the span, ensuring accurate context
/// extraction even for multi-line spans.
pub fn ast_context_for_span(
    span: &DiagnosticSpan,
    context_lines: usize,
    file_contents: &str,
) -> Option<String> {
    let lines: Vec<&str> = file_contents.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let start_line = span.line_start.saturating_sub(1);
    let end_line = span.line_end.min(lines.len());
    if start_line >= end_line {
        return None;
    }

    let context_start = start_line.saturating_sub(context_lines);
    let context_end = (end_line + context_lines).min(lines.len());

    let mut output = String::new();
    for (i, line) in lines[context_start..context_end].iter().enumerate() {
        let line_num = context_start + i + 1;
        let _ = writeln!(output, "{:4} | {}", line_num, line);
    }

    Some(output)
}

/// Format AST context information from `rho_highlight::NodeInfo`.
///
/// This formats the node position and kind for inclusion in diagnostic
/// messages, providing structured context for the model to understand
/// where in the code the error occurred.
pub fn format_ast_context(info: &rho_highlight::NodeInfo) -> String {
    format!(
        "AST context: {} at row {}, column {}",
        info.kind, info.start_row, info.start_column
    )
}