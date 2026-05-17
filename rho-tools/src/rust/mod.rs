//! Rust tooling: Cargo tools, diagnostic parsing, and rustdoc lookup.
//!
//! This module provides tools for working with Rust projects:
//!
//! - [`CargoCheck`] — Run `cargo check` and return structured diagnostics
//! - [`CargoClippy`] — Run `cargo clippy` and return lint diagnostics  
//! - [`CargoTest`] — Run `cargo test` and report test results
//! - [`CargoFix`] — Apply machine-applicable compiler suggestions
//! - [`RustcExplain`] — Get explanations for Rust error codes
//! - [`RustdocTool`] — Look up stdlib documentation from local rustdoc HTML
//!
//! ## Diagnostic parsing
//!
//! The [`parse_cargo_diagnostics`] function parses cargo `--message-format=json`
//! NDJSON output into structured [`Diagnostic`] values from `rho-core`.
//!
//! ## AST context
//!
//! The [`ast_context_for_span`] function extracts source context around a
//! diagnostic span, using tree-sitter via `rho-highlight` for accurate
//! positioning.

// Re-export core diagnostic types for downstream convenience.
pub use rho_core::diagnostic::{
    Diagnostic as RustDiagnostic, DiagnosticLevel as RustDiagnosticLevel,
    DiagnosticSpan as RustDiagnosticSpan, DiagnosticSuggestion as RustDiagnosticSuggestion,
    SuggestionApplicability as RustSuggestionApplicability,
};

// Re-export tools
pub use rustdoc::RustdocTool;
pub use tools::{CargoCheck, CargoClippy, CargoFix, CargoTest, RustcExplain};

// Re-export public parsing functions
pub use parse::parse_cargo_diagnostics;

// Re-export public formatting functions
pub use format::{ast_context_for_span, format_ast_context};

// Internal modules
mod convert;
mod format;
mod parse;
mod rustdoc;
mod tools;
mod types;
