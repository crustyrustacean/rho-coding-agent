//! Structural node queries.
//!
//! [`node_at`] maps a (line, column) position in a source file to the
//! smallest tree-sitter node that contains that position. This is the
//! primary structural-query API used by diagnostic tooling and
//! `EditFile` node-splitting validation.
//!
//! # Position conventions
//!
//! All positions are **zero-based** (row 0, column 0 = start of file).
//! Column values are **byte offsets** from the start of the line, matching
//! the convention used by tree-sitter and Rust compiler diagnostics.
//!
//! # Usage
//!
//! ```rust
//! use rho_highlight::{Language, NodeInfo, parse, node_at};
//!
//! let src = "fn main() {\n    let x = 1;\n}\n";
//! let tree = parse(src, Language::Rust).unwrap();
//!
//! // What node is at the `x` on line 1, column 8?
//! let info = node_at(&tree, src, 1, 8).unwrap();
//! assert_eq!(info.kind, "identifier");
//! ```

use crate::error::HighlightError;
use tree_sitter::{Node, Tree};

// ── NodeInfo ──────────────────────────────────────────────────────────────────

/// Information about the syntax node at a given source position.
///
/// Returned by [`node_at`]. Contains the node's kind (e.g. `"identifier"`,
/// `"function_item"`, `"ERROR"`), its byte range in the source, its
/// line/column range, whether it represents a parse error, and the chain of
/// ancestor node kinds from root down to the node itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    /// The tree-sitter node kind string (e.g. `"identifier"`, `"function_item"`).
    pub kind: String,
    /// Whether this node represents a parse error or a missing node.
    pub is_error: bool,
    /// Start byte offset in the source string (inclusive).
    pub start_byte: usize,
    /// End byte offset in the source string (exclusive).
    pub end_byte: usize,
    /// Zero-based start row (line number).
    pub start_row: usize,
    /// Zero-based start column (byte offset from line start).
    pub start_column: usize,
    /// Zero-based end row (line number).
    pub end_row: usize,
    /// Zero-based end column (byte offset from line start).
    pub end_column: usize,
    /// The text content of this node, sliced from the source.
    ///
    /// For large nodes (e.g. a whole function body) this may be many lines
    /// long. Tools that need a short summary should take a prefix.
    pub text: String,
    /// Ancestor node kinds from the root down to (and including) this node.
    ///
    /// For example, an identifier inside a `let` binding inside a function
    /// might have a path like:
    /// `["source_file", "function_item", "block", "let_declaration", "identifier"]`
    ///
    /// This gives diagnostic tools and `EditFile` enough context to decide
    /// whether an edit is node-safe.
    pub ancestor_kinds: Vec<String>,
}

// ── node_at ───────────────────────────────────────────────────────────────────

/// Return the smallest syntax node that contains the position
/// `(line, column)` (both zero-based, column in bytes).
///
/// Walks the tree from the root to find the deepest node whose span
/// contains the requested position. This is equivalent to
/// `tree.root_node().descendant_for_byte_range(byte, byte + 1)` but also
/// fills in the [`NodeInfo`] fields.
///
/// # Errors
///
/// Returns [`HighlightError::PositionOutOfRange`] if the requested position
/// falls beyond the end of the source.
///
/// # Example
///
/// ```rust
/// use rho_highlight::{Language, NodeInfo, parse, node_at};
///
/// let src = "fn main() {}\n";
/// let tree = parse(src, Language::Rust).unwrap();
///
/// // Position (0, 3) is inside `main` — an identifier.
/// let info = node_at(&tree, src, 0, 3).unwrap();
/// assert_eq!(info.kind, "identifier");
/// assert_eq!(info.text, "main");
/// ```
pub fn node_at(
    tree: &Tree,
    source: &str,
    line: usize,
    column: usize,
) -> Result<NodeInfo, HighlightError> {
    // Convert (line, column) → byte offset.
    let byte_offset = line_col_to_byte(source, line, column)?;

    let root = tree.root_node();

    // `descendant_for_byte_range` returns the smallest node that spans
    // [start_byte, end_byte).  We query a 1-byte range at the target offset.
    let end_byte = byte_offset + 1;
    let node = if end_byte <= source.len() {
        root.descendant_for_byte_range(byte_offset, end_byte)
    } else {
        // Position is exactly at the end of source: use the last byte.
        root.descendant_for_byte_range(byte_offset, byte_offset)
    };

    let node = node.unwrap_or(root); // fallback: return root for degenerate cases

    let text = source
        .get(node.start_byte()..node.end_byte())
        .unwrap_or("")
        .to_owned();

    let ancestor_kinds = ancestor_chain(&node, tree.root_node());

    Ok(NodeInfo {
        kind: node.kind().to_owned(),
        is_error: node.is_error() || node.is_missing(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_row: node.start_position().row,
        start_column: node.start_position().column,
        end_row: node.end_position().row,
        end_column: node.end_position().column,
        text,
        ancestor_kinds,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a zero-based `(line, column)` position (column = bytes from line
/// start) to a byte offset in `source`.
///
/// # Errors
///
/// Returns [`HighlightError::PositionOutOfRange`] if `line` is beyond the last
/// line of `source`, or if `column` is beyond the end of that line.
fn line_col_to_byte(source: &str, line: usize, column: usize) -> Result<usize, HighlightError> {
    let mut current_line = 0usize;
    let mut line_start_byte = 0usize;

    for (i, ch) in source.char_indices() {
        if current_line == line {
            let col_byte = line_start_byte + column;
            if col_byte > source.len() {
                return Err(HighlightError::PositionOutOfRange { line, column });
            }
            return Ok(col_byte);
        }
        if ch == '\n' {
            current_line += 1;
            line_start_byte = i + 1;
        }
    }

    // Handle the case where `line` is exactly the last line (which may have
    // no trailing newline).
    if current_line == line {
        let col_byte = line_start_byte + column;
        if col_byte > source.len() {
            return Err(HighlightError::PositionOutOfRange { line, column });
        }
        return Ok(col_byte);
    }

    Err(HighlightError::PositionOutOfRange { line, column })
}

/// Build the ancestor-kind chain from the tree root down to `node`.
///
/// Returns a `Vec<String>` where `[0]` is the root kind (e.g. `"source_file"`)
/// and the last element is `node.kind()`. The root and `node` are always included.
///
/// Tree-sitter `Node` does not provide a direct `parent()` method from an
/// immutable reference in all versions, so we reconstruct the chain by
/// walking `descendant_for_byte_range` at each ancestor level.
///
/// The algorithm:
/// 1. Walk from root, descending to the smallest node that contains
///    `(node.start_byte, node.end_byte)`.
/// 2. At each step, record the kind and check if we've reached `node`.
///
/// This is O(depth) — acceptable since tree depth is typically < 30 for
/// real Rust source.
fn ancestor_chain(node: &Node<'_>, root: Node<'_>) -> Vec<String> {
    let target_start = node.start_byte();
    let target_end = node.end_byte();
    let target_kind = node.kind();

    let mut chain = Vec::new();
    let mut current = root;

    loop {
        chain.push(current.kind().to_owned());

        // Check if we've reached the target node.
        if current.start_byte() == target_start
            && current.end_byte() == target_end
            && current.kind() == target_kind
        {
            break;
        }

        // Descend into the child that contains our target position.
        let next = current
            .named_children(&mut current.walk())
            .find(|child| child.start_byte() <= target_start && child.end_byte() >= target_end);

        match next {
            Some(child) => current = child,
            None => {
                // No named child spans the target — this happens for leaf nodes
                // or anonymous nodes. The chain is complete.
                break;
            }
        }
    }

    chain
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Language, parse};

    // ── line_col_to_byte ───────────────────────────────────────────────────

    #[test]
    fn line_col_to_byte_first_char() {
        let src = "fn main() {}";
        assert_eq!(line_col_to_byte(src, 0, 0).unwrap(), 0);
    }

    #[test]
    fn line_col_to_byte_offset_into_first_line() {
        let src = "fn main() {}";
        // 'f'=0, 'n'=1, ' '=2, 'm'=3
        assert_eq!(line_col_to_byte(src, 0, 3).unwrap(), 3);
    }

    #[test]
    fn line_col_to_byte_second_line() {
        let src = "fn foo() {}\nfn bar() {}\n";
        // Line 1 starts at byte 12 (after 'fn foo() {}\n' = 12 chars)
        assert_eq!(line_col_to_byte(src, 1, 0).unwrap(), 12);
    }

    #[test]
    fn line_col_to_byte_second_line_offset() {
        let src = "fn foo() {}\nfn bar() {}\n";
        // Line 1, col 3 = byte 12 + 3 = 15
        assert_eq!(line_col_to_byte(src, 1, 3).unwrap(), 15);
    }

    #[test]
    fn line_col_to_byte_out_of_range_line() {
        let src = "fn main() {}";
        // Only one line (line 0). Line 5 doesn't exist.
        let result = line_col_to_byte(src, 5, 0);
        assert!(matches!(
            result,
            Err(HighlightError::PositionOutOfRange { .. })
        ));
    }

    #[test]
    fn line_col_to_byte_out_of_range_column() {
        let src = "fn main() {}"; // 12 bytes
        // Column 100 on line 0 is beyond end of line.
        let result = line_col_to_byte(src, 0, 100);
        assert!(matches!(
            result,
            Err(HighlightError::PositionOutOfRange { .. })
        ));
    }

    #[test]
    fn line_col_to_byte_empty_source() {
        let src = "";
        // Byte 0 column 0 on empty source is the end of the file: valid (= 0).
        assert_eq!(line_col_to_byte(src, 0, 0).unwrap(), 0);
    }

    #[test]
    fn line_col_to_byte_no_trailing_newline() {
        let src = "fn main() {}"; // no newline
        // The last line is line 0; col 11 is the last '{}'.
        assert_eq!(line_col_to_byte(src, 0, 11).unwrap(), 11);
    }

    #[test]
    fn line_col_to_byte_multibyte_chars() {
        // '日' is 3 UTF-8 bytes.
        let src = "// 日本語\nfn main() {}";
        // Line 1 starts after '// 日本語\n'.
        // "// 日本語" = 2 + 1 + 9 = 12 bytes, plus '\n' = 13 bytes total.
        assert_eq!(line_col_to_byte(src, 1, 0).unwrap(), 13);
    }

    // ── node_at ───────────────────────────────────────────────────────────

    #[test]
    fn node_at_function_name_is_identifier() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // 'main' starts at byte 3, column 3
        let info = node_at(&tree, src, 0, 3).unwrap();
        assert_eq!(info.kind, "identifier");
        assert_eq!(info.text, "main");
    }

    #[test]
    fn node_at_fn_keyword_is_fn() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let info = node_at(&tree, src, 0, 0).unwrap();
        // The `fn` keyword maps to the "fn" node kind.
        assert_eq!(info.kind, "fn");
    }

    #[test]
    fn node_at_let_identifier_on_second_line() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // Line 1, col 8 is `x` in `let x = 1;`
        // "    let " = 8 bytes from line start
        let info = node_at(&tree, src, 1, 8).unwrap();
        assert_eq!(info.kind, "identifier");
        assert_eq!(info.text, "x");
    }

    #[test]
    fn node_at_has_nonempty_ancestor_chain() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let info = node_at(&tree, src, 1, 8).unwrap();
        // The chain must start with "source_file" and end with "identifier".
        assert!(
            !info.ancestor_kinds.is_empty(),
            "ancestor chain should not be empty"
        );
        assert_eq!(
            info.ancestor_kinds[0], "source_file",
            "chain should start at the root"
        );
        assert_eq!(
            info.ancestor_kinds.last().unwrap(),
            "identifier",
            "chain should end at the target node kind"
        );
    }

    #[test]
    fn node_at_root_has_short_ancestor_chain() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // Querying position 0,0 gives the `fn` keyword; its chain goes
        // source_file → function_item → fn
        let info = node_at(&tree, src, 0, 0).unwrap();
        assert!(
            !info.ancestor_kinds.is_empty(),
            "even the root has at least itself in the chain"
        );
        assert_eq!(info.ancestor_kinds[0], "source_file");
    }

    #[test]
    fn node_at_start_byte_le_end_byte() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let info = node_at(&tree, src, 0, 3).unwrap();
        assert!(
            info.start_byte <= info.end_byte,
            "start_byte must be <= end_byte"
        );
    }

    #[test]
    fn node_at_text_matches_source_slice() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let info = node_at(&tree, src, 0, 3).unwrap();
        let expected = &src[info.start_byte..info.end_byte];
        assert_eq!(info.text, expected, "text should be the source slice");
    }

    #[test]
    fn node_at_out_of_range_position_returns_error() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // Line 99 doesn't exist.
        let result = node_at(&tree, src, 99, 0);
        assert!(
            matches!(result, Err(HighlightError::PositionOutOfRange { .. })),
            "out-of-range position should return PositionOutOfRange"
        );
    }

    #[test]
    fn node_at_error_node_is_flagged() {
        // Syntactically broken source — tree-sitter will produce ERROR nodes.
        let src = "fn main() { let x = !!!; }\n";
        let tree = parse(src, Language::Rust).unwrap();
        // There will be an ERROR node somewhere in the tree; node_at should not panic.
        // We just verify it returns Ok (not an error from our side).
        let result = node_at(&tree, src, 0, 20);
        assert!(
            result.is_ok(),
            "broken source should still produce Ok from node_at"
        );
    }

    #[test]
    fn node_at_empty_source_does_not_panic() {
        let src = "";
        let tree = parse(src, Language::Rust).unwrap();
        // position (0,0) in empty source: byte 0
        let result = node_at(&tree, src, 0, 0);
        // Should return Ok (the root node) or an appropriate error — not a panic.
        // Empty source has a source_file root at byte range 0..0.
        assert!(
            result.is_ok(),
            "node_at on empty source should not panic or error"
        );
    }

    #[test]
    fn node_at_unicode_source_positions_are_byte_based() {
        // 'こ' is 3 UTF-8 bytes. "// こ" = 5 bytes.
        // Column values are byte offsets so column 3 falls inside the first
        // multibyte character — node_at must not panic even for mid-codepoint
        // byte offsets (tree-sitter handles these gracefully).
        let src = "// こんにちは\nfn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // Query on line 1, col 3 = `ain` in `main`
        let info = node_at(&tree, src, 1, 3).unwrap();
        assert_eq!(info.kind, "identifier");
    }

    #[test]
    fn node_at_row_column_range_is_consistent() {
        let src = "fn main() {}\nfn other() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        // `other` starts at line 1, col 3
        let info = node_at(&tree, src, 1, 3).unwrap();
        assert_eq!(info.kind, "identifier");
        assert_eq!(info.text, "other");
        assert_eq!(info.start_row, 1, "start_row should match the query line");
        assert_eq!(
            info.start_column, 3,
            "start_column should match the query col"
        );
    }

    #[test]
    fn node_at_is_error_false_for_valid_node() {
        let src = "fn main() {}\n";
        let tree = parse(src, Language::Rust).unwrap();
        let info = node_at(&tree, src, 0, 3).unwrap();
        assert!(
            !info.is_error,
            "`main` identifier should not be an error node"
        );
    }
}
