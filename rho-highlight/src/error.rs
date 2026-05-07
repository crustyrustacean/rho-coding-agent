//! Error types for rho-highlight.

use thiserror::Error;

/// Errors that can occur in rho-highlight operations.
#[derive(Debug, Error)]
pub enum HighlightError {
    /// The requested grammar is not compiled into this binary.
    ///
    /// Enable the corresponding Cargo feature (e.g. `rust`, `toml`) and
    /// rebuild.
    #[error("grammar not available: {0} (enable the feature flag and rebuild)")]
    GrammarNotAvailable(String),

    /// The tree-sitter parser failed to produce a tree for the given source.
    ///
    /// This is unusual — tree-sitter produces partial trees even for invalid
    /// syntax — but can occur on extremely malformed input or internal parser
    /// errors.
    #[error("parse failed for {language}: {reason}")]
    ParseFailed {
        /// The language that was being parsed.
        language: String,
        /// A description of why parsing failed.
        reason: String,
    },

    /// A structural query (e.g. [`node_at`](crate::query::node_at)) was called
    /// with a position that falls outside the source range.
    #[error("position ({line}, {column}) is out of range for the given source")]
    PositionOutOfRange {
        /// Zero-based line number.
        line: usize,
        /// Zero-based column number (bytes from line start).
        column: usize,
    },
}
