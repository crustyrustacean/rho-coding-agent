//! Source parsing: produce a tree-sitter [`Tree`](tree_sitter::Tree) from a
//! source string.
//!
//! # Usage
//!
//! ```rust
//! use rho_highlight::{Language, parse};
//!
//! let tree = parse("fn main() {}", Language::Rust).expect("parse must succeed");
//! assert_eq!(tree.root_node().kind(), "source_file");
//! ```
//!
//! # Partial trees
//!
//! Tree-sitter always produces a tree, even for syntactically invalid input.
//! Nodes that could not be parsed have [`Node::is_error`](tree_sitter::Node::is_error)
//! or [`Node::is_missing`](tree_sitter::Node::is_missing) set to `true`. The
//! caller may inspect these to detect parse errors.
//!
//! # Encoding
//!
//! Tree-sitter operates on raw bytes. This module always passes UTF-8 encoded
//! source. Row and column positions returned by tree-sitter are byte-based.
//! The [`query`](crate::query) module accounts for this when mapping line/column
//! coordinates to node positions.

use crate::error::HighlightError;
use crate::lang::Language;

/// Parse `source` using the grammar for `language` and return the syntax tree.
///
/// # Errors
///
/// - [`HighlightError::GrammarNotAvailable`] — the requested grammar's Cargo
///   feature is not enabled.
/// - [`HighlightError::ParseFailed`] — tree-sitter failed to produce a tree
///   (very rare; typically indicates corrupt or excessively large input).
///
/// # Example
///
/// ```rust
/// use rho_highlight::{Language, parse};
///
/// let tree = parse("fn main() {}", Language::Rust).unwrap();
/// assert_eq!(tree.root_node().kind(), "source_file");
/// ```
pub fn parse(source: &str, language: Language) -> Result<tree_sitter::Tree, HighlightError> {
    let ts_language = language.tree_sitter_language()?;

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| HighlightError::ParseFailed {
            language: language.name().to_owned(),
            reason: e.to_string(),
        })?;

    parser
        .parse(source.as_bytes(), None)
        .ok_or_else(|| HighlightError::ParseFailed {
            language: language.name().to_owned(),
            reason: "parser returned None (possible timeout or cancellation)".to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Rust grammar ───────────────────────────────────────────────────────

    #[test]
    fn parse_empty_string_returns_tree() {
        let tree = parse("", Language::Rust).expect("empty source should produce a tree");
        // An empty Rust file is a valid source_file with no children.
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_simple_function_returns_source_file() {
        let src = "fn main() {}";
        let tree = parse(src, Language::Rust).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_hello_world_has_no_errors() {
        let src = r#"fn main() { println!("Hello, world!"); }"#;
        let tree = parse(src, Language::Rust).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "well-formed source should not have parse errors"
        );
    }

    #[test]
    fn parse_multi_item_file_is_source_file() {
        let src = "use std::fmt;\n\nstruct Foo {\n    x: i32,\n}\n\nimpl fmt::Display for Foo {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        write!(f, \"{}\", self.x)\n    }\n}\n";
        let tree = parse(src, Language::Rust).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn parse_invalid_syntax_still_returns_tree() {
        // Tree-sitter produces a partial tree even for invalid input.
        let src = "fn main() { let x = !!!"; // syntactically broken
        let result = parse(src, Language::Rust);
        assert!(result.is_ok(), "broken source should still produce a tree");
        // The tree will contain ERROR nodes.
        let tree = result.unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_multiline_source_preserves_source_range() {
        let src = "fn foo() {}\nfn bar() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let root = tree.root_node();
        assert_eq!(root.start_position().row, 0);
        // Two functions — root spans to row 2
        assert_eq!(root.end_position().row, 2);
    }

    // ── Grammar not available ──────────────────────────────────────────────

    #[test]
    fn parse_unavailable_grammar_returns_error() {
        let result = parse("source", Language::PowerShell);
        assert!(
            matches!(result, Err(HighlightError::GrammarNotAvailable(_))),
            "should return GrammarNotAvailable for un-featured grammars"
        );
    }

    // ── Unicode source ─────────────────────────────────────────────────────

    #[test]
    fn parse_unicode_identifiers() {
        // Rust allows non-ASCII in string literals and comments.
        let src = r#"fn main() { let _x = "こんにちは世界"; }"#;
        let tree = parse(src, Language::Rust).unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_unicode_comment() {
        let src = "// こんにちは\nfn main() {}";
        let tree = parse(src, Language::Rust).unwrap();
        assert!(!tree.root_node().has_error());
    }
}
