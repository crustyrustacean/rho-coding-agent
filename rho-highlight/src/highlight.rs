//! Syntax highlighting: produce classified [`HighlightSpan`]s from source.
//!
//! # Phase 3 scope
//!
//! This module produces **classified spans** only — each span carries a
//! [`HighlightTag`] (e.g. `Keyword`, `String`, `Comment`) and the byte range
//! it covers. Theme and colour mapping (ANSI escape codes, `ratatui` styles)
//! are a **Phase 4 concern** and are intentionally absent here.
//!
//! # Algorithm
//!
//! The implementation uses tree-sitter's node-kind strings to classify tokens.
//! This is simpler and more portable than tree-sitter's highlight query system
//! (which requires `.scm` query files) and produces classifications accurate
//! enough for Phase 3's needs (diagnostic context rendering, `EditFile`
//! node-type identification). A query-based highlighter can be layered on top
//! in Phase 4.
//!
//! The walk visits every **named** leaf node in the syntax tree and maps its
//! `kind` string to a [`HighlightTag`]. Anonymous nodes (punctuation, operators
//! stored as string literals in the grammar) are tagged as [`HighlightTag::Other`].
//!
//! # Usage
//!
//! ```rust
//! use rho_highlight::{Language, HighlightTag, highlight};
//!
//! let src = r#"fn main() { println!("hello"); }"#;
//! let spans = highlight(src, Language::Rust).unwrap();
//!
//! // There must be at least one Keyword span (the `fn` keyword).
//! assert!(spans.iter().any(|s| s.tag == HighlightTag::Keyword));
//! ```

use crate::error::HighlightError;
use crate::lang::Language;
use crate::parse::parse;

// ── HighlightTag ──────────────────────────────────────────────────────────────

/// A semantic classification for a source token.
///
/// These are broad, stable categories suitable for syntax highlighting.
/// Phase 4 may introduce finer-grained tags (e.g. `FunctionName`,
/// `TypeName`, `LifetimeName`) when the query-based highlighter lands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HighlightTag {
    /// Language keyword (`fn`, `let`, `pub`, `use`, `struct`, etc.).
    Keyword,
    /// String literal (double-quoted, raw string, byte string).
    StringLiteral,
    /// Integer or float literal.
    NumberLiteral,
    /// Boolean literal (`true`, `false`).
    BoolLiteral,
    /// Character literal (`'a'`, `'\n'`).
    CharLiteral,
    /// Line comment (`//`) or block comment (`/* */`).
    Comment,
    /// An identifier (variable name, function name, type name, etc.).
    Identifier,
    /// A macro invocation name or macro definition name.
    Macro,
    /// A lifetime label (`'a`, `'static`).
    Lifetime,
    /// An attribute (`#[derive(…)]`, `#![allow(…)]`).
    Attribute,
    /// A type name or primitive type keyword (`i32`, `usize`, `str`).
    Type,
    /// A punctuation or operator token, or an unrecognised anonymous node.
    Other,
}

impl HighlightTag {
    /// A short, stable label for this tag, useful for testing and serialization.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::StringLiteral => "string",
            Self::NumberLiteral => "number",
            Self::BoolLiteral => "bool",
            Self::CharLiteral => "char",
            Self::Comment => "comment",
            Self::Identifier => "identifier",
            Self::Macro => "macro",
            Self::Lifetime => "lifetime",
            Self::Attribute => "attribute",
            Self::Type => "type",
            Self::Other => "other",
        }
    }
}

impl std::fmt::Display for HighlightTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ── HighlightSpan ─────────────────────────────────────────────────────────────

/// A classified byte-range within a source string.
///
/// The `start_byte..end_byte` range is a valid slice into the original
/// `source` string passed to [`highlight`]. Both ends are byte indices
/// (not character indices).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    /// The semantic classification of this token.
    pub tag: HighlightTag,
    /// Start byte offset in the source (inclusive).
    pub start_byte: usize,
    /// End byte offset in the source (exclusive).
    pub end_byte: usize,
    /// Zero-based line number of the start of this span.
    pub start_row: usize,
    /// Zero-based column (byte offset from line start) of the start.
    pub start_column: usize,
}

impl HighlightSpan {
    /// Slice the source string to get the text covered by this span.
    ///
    /// Returns `""` if the byte range is out of bounds (shouldn't happen
    /// with spans produced by [`highlight`]).
    pub fn text<'s>(&self, source: &'s str) -> &'s str {
        source
            .get(self.start_byte..self.end_byte)
            .unwrap_or_default()
    }
}

// ── highlight ─────────────────────────────────────────────────────────────────

/// Produce syntax highlight spans for `source` using the `language` grammar.
///
/// Returns a list of [`HighlightSpan`]s in source order (by `start_byte`).
/// Spans do not overlap. Gaps between spans are unlabelled tokens (anonymous
/// punctuation, whitespace) that callers may render without colour.
///
/// # Errors
///
/// - [`HighlightError::GrammarNotAvailable`] — the grammar feature is not enabled.
/// - [`HighlightError::ParseFailed`] — tree-sitter failed to produce a tree.
///
/// # Example
///
/// ```rust
/// use rho_highlight::{Language, HighlightTag, highlight};
///
/// let src = "fn main() {}";
/// let spans = highlight(src, Language::Rust).unwrap();
///
/// let keywords: Vec<_> = spans.iter()
///     .filter(|s| s.tag == HighlightTag::Keyword)
///     .collect();
/// assert!(!keywords.is_empty());
/// ```
pub fn highlight(source: &str, language: Language) -> Result<Vec<HighlightSpan>, HighlightError> {
    let tree = parse(source, language)?;
    let mut spans = Vec::new();
    walk_tree(tree.root_node(), &mut spans);
    spans.sort_by_key(|s| s.start_byte);
    Ok(spans)
}

// ── Tree walk ─────────────────────────────────────────────────────────────────

/// Named node kinds that should be emitted as a single span covering their
/// entire byte range, rather than recursed into leaf-by-leaf.
///
/// These are *compound* nodes whose leaves don't individually carry semantic
/// meaning (e.g. a `string_literal` node consists of `"`, `string_content`,
/// `"` leaves — we want one `StringLiteral` span, not three `Other` spans).
fn classify_compound(node: &tree_sitter::Node<'_>) -> Option<HighlightTag> {
    if !node.is_named() {
        return None;
    }
    Some(match node.kind() {
        // Comments — tree-sitter-rust emits `line_comment` / `block_comment`
        // as named nodes with the entire comment as their range.
        "line_comment" | "block_comment" => HighlightTag::Comment,

        // String literals decompose into '"', string_content, '"' leaves.
        // Emit the whole node as one span.
        "string_literal"
        | "raw_string_literal"
        | "byte_string_literal"
        | "raw_byte_string_literal"
        | "c_string_literal"
        | "raw_c_string_literal" => HighlightTag::StringLiteral,

        // Boolean literals: `boolean_literal` is a named node whose single
        // anonymous child is the keyword `true` or `false`.
        "boolean_literal" => HighlightTag::BoolLiteral,

        // Character literals: `char_literal` wraps `'`, char content, `'`.
        "char_literal" | "byte_literal" => HighlightTag::CharLiteral,

        // Attributes: `attribute_item` / `inner_attribute_item` wrap everything.
        "attribute_item" | "inner_attribute_item" => HighlightTag::Attribute,

        // Lifetime: `lifetime` wraps the `'` + `identifier` leaves.
        "lifetime" => HighlightTag::Lifetime,

        _ => return None,
    })
}

/// Recursively walk the tree and collect highlight spans.
///
/// Compound named nodes (strings, comments, lifetimes, …) are emitted as
/// single spans. Simple leaf nodes are classified individually.
fn walk_tree(node: tree_sitter::Node<'_>, spans: &mut Vec<HighlightSpan>) {
    // Skip zero-width nodes.
    if node.start_byte() == node.end_byte() {
        return;
    }

    // Compound named nodes — emit as a single span and stop recursing.
    if let Some(tag) = classify_compound(&node) {
        spans.push(HighlightSpan {
            tag,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: node.start_position().row,
            start_column: node.start_position().column,
        });
        return;
    }

    if node.child_count() == 0 {
        // Leaf node — classify it.
        if let Some(tag) = classify_leaf(&node) {
            spans.push(HighlightSpan {
                tag,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_row: node.start_position().row,
                start_column: node.start_position().column,
            });
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree(child, spans);
    }
}

/// Classify a leaf node (zero children).
fn classify_leaf(node: &tree_sitter::Node<'_>) -> Option<HighlightTag> {
    let kind = node.kind();

    // ── Named leaf nodes ──────────────────────────────────────────────────
    if node.is_named() {
        return Some(match kind {
            // Number literals are always leaf nodes.
            "integer_literal" | "float_literal" => HighlightTag::NumberLiteral,

            // Identifiers (variable, function, field).
            "identifier" | "field_identifier" => HighlightTag::Identifier,

            // Type identifiers and primitive types.
            "type_identifier" | "primitive_type" => HighlightTag::Type,

            // `string_content` appears inside string literals; handled at the
            // compound level, but guard here in case walk reaches it somehow.
            "string_content" => HighlightTag::StringLiteral,

            _ => HighlightTag::Other,
        });
    }

    // ── Anonymous leaf nodes (keywords, operators, punctuation) ──────────
    // Anonymous node `kind` is the literal token string.
    Some(match kind {
        // Rust keywords
        "fn" | "let" | "pub" | "use" | "mod" | "crate" | "super" | "self" | "Self" | "struct"
        | "enum" | "trait" | "impl" | "type" | "const" | "static" | "mut" | "ref" | "if"
        | "else" | "match" | "for" | "while" | "loop" | "return" | "break" | "continue" | "in"
        | "where" | "async" | "await" | "move" | "dyn" | "unsafe" | "extern" | "as" | "box"
        | "yield" | "become" | "do" | "abstract" | "final" | "override" | "priv" | "typeof"
        | "unsized" | "virtual" => HighlightTag::Keyword,

        // Everything else (operators, punctuation, whitespace) — skip
        _ => return None,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── HighlightTag ──────────────────────────────────────────────────────

    #[test]
    fn tag_labels_are_unique() {
        use std::collections::HashSet;
        let tags = [
            HighlightTag::Keyword,
            HighlightTag::StringLiteral,
            HighlightTag::NumberLiteral,
            HighlightTag::BoolLiteral,
            HighlightTag::CharLiteral,
            HighlightTag::Comment,
            HighlightTag::Identifier,
            HighlightTag::Macro,
            HighlightTag::Lifetime,
            HighlightTag::Attribute,
            HighlightTag::Type,
            HighlightTag::Other,
        ];
        let labels: HashSet<_> = tags.iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), tags.len(), "all tag labels must be unique");
    }

    #[test]
    fn tag_display_matches_label() {
        assert_eq!(HighlightTag::Keyword.to_string(), "keyword");
        assert_eq!(HighlightTag::Comment.to_string(), "comment");
    }

    // ── highlight ─────────────────────────────────────────────────────────

    #[test]
    fn highlight_empty_returns_empty() {
        let spans = highlight("", Language::Rust).unwrap();
        assert!(spans.is_empty(), "empty source should produce no spans");
    }

    #[test]
    fn highlight_fn_keyword_is_present() {
        let src = "fn main() {}";
        let spans = highlight(src, Language::Rust).unwrap();
        let fn_span = spans
            .iter()
            .find(|s| s.tag == HighlightTag::Keyword && s.text(src) == "fn");
        assert!(fn_span.is_some(), "should find a keyword span for `fn`");
    }

    #[test]
    fn highlight_identifier_main_is_present() {
        let src = "fn main() {}";
        let spans = highlight(src, Language::Rust).unwrap();
        let main_span = spans
            .iter()
            .find(|s| s.tag == HighlightTag::Identifier && s.text(src) == "main");
        assert!(
            main_span.is_some(),
            "should find an identifier span for `main`"
        );
    }

    #[test]
    fn highlight_string_literal_tagged_correctly() {
        let src = r#"fn main() { let _s = "hello"; }"#;
        let spans = highlight(src, Language::Rust).unwrap();
        let has_string = spans.iter().any(|s| s.tag == HighlightTag::StringLiteral);
        assert!(has_string, "should find a StringLiteral span");
    }

    #[test]
    fn highlight_number_literal_tagged_correctly() {
        let src = "fn main() { let _n = 42; }";
        let spans = highlight(src, Language::Rust).unwrap();
        let num = spans
            .iter()
            .find(|s| s.tag == HighlightTag::NumberLiteral && s.text(src) == "42");
        assert!(num.is_some(), "should find a NumberLiteral span for `42`");
    }

    #[test]
    fn highlight_bool_literal_tagged_correctly() {
        let src = "fn main() { let _b = true; }";
        let spans = highlight(src, Language::Rust).unwrap();
        let has_bool = spans.iter().any(|s| s.tag == HighlightTag::BoolLiteral);
        assert!(has_bool, "should find a BoolLiteral span for `true`");
    }

    #[test]
    fn highlight_comment_tagged_correctly() {
        let src = "// this is a comment\nfn main() {}";
        let spans = highlight(src, Language::Rust).unwrap();
        let has_comment = spans.iter().any(|s| s.tag == HighlightTag::Comment);
        assert!(has_comment, "should find a Comment span");
    }

    #[test]
    fn highlight_spans_are_in_byte_order() {
        let src = r#"fn main() { let _s = "hello"; }"#;
        let spans = highlight(src, Language::Rust).unwrap();
        for window in spans.windows(2) {
            assert!(
                window[0].start_byte <= window[1].start_byte,
                "spans should be in ascending byte order"
            );
        }
    }

    #[test]
    fn highlight_span_ranges_do_not_overlap() {
        let src = "fn main() { let x = 42; }";
        let spans = highlight(src, Language::Rust).unwrap();
        for window in spans.windows(2) {
            assert!(
                window[0].end_byte <= window[1].start_byte,
                "spans must not overlap: {:?} and {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn highlight_span_text_matches_source() {
        let src = "fn main() {}";
        let spans = highlight(src, Language::Rust).unwrap();
        for span in &spans {
            let text = &src[span.start_byte..span.end_byte];
            assert_eq!(
                span.text(src),
                text,
                "span.text() should match source slice"
            );
        }
    }

    #[test]
    fn highlight_type_identifier_tagged() {
        let src = "struct Foo { x: i32 }";
        let spans = highlight(src, Language::Rust).unwrap();
        let has_type = spans
            .iter()
            .any(|s| s.tag == HighlightTag::Type && (s.text(src) == "Foo" || s.text(src) == "i32"));
        assert!(has_type, "should find a Type span for Foo or i32");
    }

    #[test]
    fn highlight_unavailable_grammar_returns_error() {
        let result = highlight("source", Language::PowerShell);
        assert!(
            matches!(result, Err(HighlightError::GrammarNotAvailable(_))),
            "should return GrammarNotAvailable for un-featured language"
        );
    }

    #[test]
    fn highlight_lifetime_span_found() {
        let src = "fn foo<'a>(x: &'a str) {}";
        let spans = highlight(src, Language::Rust).unwrap();
        let has_lifetime = spans.iter().any(|s| s.tag == HighlightTag::Lifetime);
        assert!(has_lifetime, "should find a Lifetime span for `'a`");
    }

    #[test]
    fn highlight_char_literal_tagged() {
        let src = "fn main() { let _c = 'z'; }";
        let spans = highlight(src, Language::Rust).unwrap();
        let has_char = spans.iter().any(|s| s.tag == HighlightTag::CharLiteral);
        assert!(has_char, "should find a CharLiteral span");
    }

    #[test]
    fn highlight_let_keyword_is_keyword() {
        let src = "fn main() { let _x = 1; }";
        let spans = highlight(src, Language::Rust).unwrap();
        let let_kw = spans
            .iter()
            .find(|s| s.tag == HighlightTag::Keyword && s.text(src) == "let");
        assert!(let_kw.is_some(), "should find a Keyword span for `let`");
    }
}
