//! Hashline computation module for content-addressed line editing.
//!
//! Provides hash computation for file lines using a custom 2-character hash
//! from a 16-letter alphabet. Hashes are deterministic based on line content
//! (for lines with alphanumerics) or line number (for purely punctuation lines).
//!
//! # Hash Algorithm
//!
//! - Lines with alphanumeric characters: Hash based on line content
//! - Lines without alphanumerics: Hash based on line number
//! - 2-character hash from alphabet: `ZPMQVRWSNKTXJBYH`
//! - Deterministic: Same input always produces same output

/// Custom alphabet for hash characters (excludes hex, vowels, ambiguous letters).
const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH";

/// Compute a 2-character hash for a line.
///
/// # Arguments
///
/// * `line` - The line content (without trailing newline)
/// * `line_num` - The line number (1-indexed)
///
/// # Returns
///
/// A 2-character string from the custom alphabet.
///
/// # Hash Logic
///
/// - If the line contains any alphanumeric characters, compute hash from content
/// - Otherwise, use the line number for deterministic hashing
///
/// # Panics
///
/// Cannot panic in practice: `ALPHABET` is valid UTF-8 and the indices are
/// masked to the 0..16 range.
pub fn compute_line_hash(line: &str, line_num: usize) -> String {
    let seed = if line.chars().any(char::is_alphanumeric) {
        // Simple hash based on line content
        line.bytes().fold(0u32, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u32::from(b))
        })
    } else {
        // Use line number for lines without alphanumerics (e.g., }{)())
        u32::try_from(line_num).unwrap_or(u32::MAX)
    };

    let idx1 = (seed & 0x0F) as usize;
    let idx2 = ((seed >> 4) & 0x0F) as usize;

    // ALPHABET only contains valid UTF-8 ASCII characters
    let bytes = [ALPHABET[idx1], ALPHABET[idx2]];
    String::from_utf8(bytes.to_vec()).expect("ALPHABET contains valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_output_for_same_input() {
        let line = "function hello() {";
        let hash1 = compute_line_hash(line, 1);
        let hash2 = compute_line_hash(line, 1);
        assert_eq!(hash1, hash2, "Hash should be deterministic");
    }

    #[test]
    fn test_different_hashes_for_different_lines() {
        let hash1 = compute_line_hash("function hello() {", 1);
        let hash2 = compute_line_hash("    console.log(\"world\");", 2);
        assert_ne!(
            hash1, hash2,
            "Different lines should produce different hashes"
        );
    }

    #[test]
    fn test_line_number_based_hashing_for_non_alphanumeric() {
        let line = "}";
        // This line has no alphanumeric characters, so hash is based on line number
        let hash1 = compute_line_hash(line, 1);
        let hash2 = compute_line_hash(line, 2);
        assert_ne!(
            hash1, hash2,
            "Line number should affect hash for non-alphanumeric lines"
        );

        // Same line number should produce same hash
        let hash3 = compute_line_hash(")", 1);
        assert_eq!(
            hash1, hash3,
            "Same line number should produce same hash for different non-alphanumeric lines"
        );
    }

    #[test]
    fn test_content_based_hashing_for_alphanumeric_lines() {
        // Lines with alphanumerics should hash based on content, not line number
        let line = "function hello() {";
        let hash1 = compute_line_hash(line, 1);
        let hash2 = compute_line_hash(line, 100);
        assert_eq!(
            hash1, hash2,
            "Alphanumeric lines should hash based on content, not line number"
        );
    }

    #[test]
    fn test_hash_length() {
        let line = "function hello() {";
        let hash = compute_line_hash(line, 1);
        assert_eq!(hash.len(), 2, "Hash should be exactly 2 characters");
    }

    #[test]
    fn test_alphabet_coverage() {
        let mut seen = std::collections::HashSet::new();

        // Generate hashes for various inputs to cover alphabet
        let test_cases = vec![
            "a",
            "b",
            "c",
            "d",
            "e",
            "f",
            "g",
            "h",
            "i",
            "j",
            "k",
            "l",
            "m",
            "n",
            "o",
            "p",
            "function",
            "class",
            "import",
            "export",
            "const",
            "let",
            "var",
            "if",
            "else",
            "for",
            "while",
            "return",
            "break",
            "continue",
            "try",
            "catch",
            "finally",
            "throw",
            "new",
            "this",
            "super",
            "extends",
            "static",
            "public",
            "private",
            "protected",
            "readonly",
            "async",
            "await",
            "yield",
            "typeof",
            "instanceof",
            "void",
            "null",
            "undefined",
            "true",
            "false",
            "0",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "hello world",
            "foo bar baz",
            "test",
            "example",
            "sample",
            "demo",
        ];

        for (i, line) in test_cases.iter().enumerate() {
            let hash = compute_line_hash(line, i);
            for char in hash.chars() {
                seen.insert(char);
            }
        }

        // We should see all 16 characters with varied input
        assert_eq!(
            seen.len(),
            16,
            "All 16 alphabet characters should be used with varied input"
        );
    }

    #[test]
    fn test_alphabet_characters_only() {
        let line = "function hello() {";
        let hash = compute_line_hash(line, 1);

        for char in hash.chars() {
            assert!(
                ALPHABET.contains(&(char as u8)),
                "Hash character '{char}' should be from the alphabet"
            );
        }
    }

    #[test]
    fn test_empty_line() {
        let hash = compute_line_hash("", 1);
        assert_eq!(
            hash.len(),
            2,
            "Empty line should still produce 2-character hash"
        );
    }

    #[test]
    fn test_whitespace_only_line() {
        let hash = compute_line_hash("    ", 1);
        assert_eq!(
            hash.len(),
            2,
            "Whitespace-only line should still produce 2-character hash"
        );
    }

    #[test]
    fn test_unicode_line() {
        let line = "const café = ☕;";
        let hash = compute_line_hash(line, 1);
        assert_eq!(
            hash.len(),
            2,
            "Unicode line should still produce 2-character hash"
        );
    }

    #[test]
    fn test_very_long_line() {
        let line = "a".repeat(1000);
        let hash = compute_line_hash(&line, 1);
        assert_eq!(
            hash.len(),
            2,
            "Very long line should still produce 2-character hash"
        );
    }
}
