//! Hashline computation module for content-addressed line editing.
//!
//! Provides hash computation for file lines using a custom 4-character hash
//! from a 16-letter alphabet (65,536 possible values). Hashes are deterministic
//! based on line content (for lines with alphanumerics) or line number (for
//! purely punctuation lines).
//!
//! # Hash Algorithm
//!
//! - Lines with alphanumeric characters: Hash based on line content
//! - Lines without alphanumerics: Hash based on line number
//!
//! The 32-bit seed is folded into 4 indices of 4 bits each, selecting from
//! a 16-character alphabet to produce a 4-character hash.
//!
//! # Collision properties
//!
//! With 65,536 possible values, the birthday-problem threshold (50% collision
//! probability) is ~302 lines. For files under ~250 lines (the vast majority
//! of edits), collision probability is under 40%. This is a dramatic improvement
//! over the original 2-character hash (256 values, 50% collision at ~19 lines).
//!
//! # TUI Integration (Phase 4)
//!
//! Hashline output can be parsed with the regex `^(\s*)(\d+)#([A-Z]{4}):(.*)$`,
//! capturing: `[whitespace, line_num, hash, content]`.
//!
//! Suggested TUI rendering:
//! - Line numbers: dimmed
//! - Hash: dimmed or hidden (toggle with a key)
//! - Content: normal foreground
//! - Anchors can be click-to-copy for manual editing
//!
//! To strip hashes for display: `line.splitn(3, ':').nth(2)`
//! - 4-character hash from alphabet: `ZPMQVRWSNKTXJBYH`
//! - Deterministic: Same input always produces same output

/// Custom alphabet for hash characters (excludes hex, vowels, ambiguous letters).
const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH";

/// Number of hash characters produced by [`compute_line_hash`].
pub const HASH_LEN: usize = 4;

/// Compute a 4-character hash for a line.
///
/// # Arguments
///
/// * `line` - The line content (without trailing newline)
/// * `line_num` - The line number (1-indexed)
///
/// # Returns
///
/// A 4-character string from the custom alphabet (65,536 possible values).
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
    let idx3 = ((seed >> 8) & 0x0F) as usize;
    let idx4 = ((seed >> 12) & 0x0F) as usize;

    // ALPHABET only contains valid UTF-8 ASCII characters
    let bytes = [
        ALPHABET[idx1],
        ALPHABET[idx2],
        ALPHABET[idx3],
        ALPHABET[idx4],
    ];
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
        assert_eq!(
            hash.len(),
            HASH_LEN,
            "Hash should be exactly {HASH_LEN} characters"
        );
    }

    #[test]
    fn test_alphabet_coverage() {
        let mut seen = std::collections::HashSet::new();

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
            HASH_LEN,
            "Empty line should still produce {HASH_LEN}-character hash"
        );
    }

    #[test]
    fn test_whitespace_only_line() {
        let hash = compute_line_hash("    ", 1);
        assert_eq!(
            hash.len(),
            HASH_LEN,
            "Whitespace-only line should still produce {HASH_LEN}-character hash"
        );
    }

    #[test]
    fn test_unicode_line() {
        let line = "const café = ☕;";
        let hash = compute_line_hash(line, 1);
        assert_eq!(
            hash.len(),
            HASH_LEN,
            "Unicode line should still produce {HASH_LEN}-character hash"
        );
    }

    #[test]
    fn test_very_long_line() {
        let line = "a".repeat(1000);
        let hash = compute_line_hash(&line, 1);
        assert_eq!(
            hash.len(),
            HASH_LEN,
            "Very long line should still produce {HASH_LEN}-character hash"
        );
    }

    #[test]
    fn test_performance_10k_lines() {
        let lines: Vec<String> = (1..=10_000)
            .map(|i| format!("fn function_{i:04}() {{ let x = {i}; }}"))
            .collect();

        let start = std::time::Instant::now();
        for (i, line) in lines.iter().enumerate() {
            let _hash = compute_line_hash(line, i + 1);
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 50,
            "10k lines took {elapsed:?} — should be < 50ms",
        );
    }

    // ── Collision rate tests ──────────────────────────────────────────────

    #[test]
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn test_collision_rate_typical_rust_file() {
        // A ~100-line Rust file should have very few collisions with 4-char hashes
        // (v1 2-char hash had 23 collisions in 106 lines).
        let lines: Vec<&str> = vec![
            "use crate::error::ApiError;",
            "use crate::models::Document;",
            "use chrono::{DateTime, Utc};",
            "use sha2::{Digest, Sha256};",
            "use sqlx::{SqlitePool, Row};",
            "use uuid::Uuid;",
            "",
            "pub struct Database {",
            "    pool: SqlitePool,",
            "}",
            "",
            "impl Database {",
            "    pub fn new(pool: SqlitePool) -> Self {",
            "        Self { pool }",
            "    }",
            "",
            "    fn compute_hash(content: &str) -> String {",
            "        let mut hasher = Sha256::new();",
            "        hasher.update(content.as_bytes());",
            "        format!(\"{:x}\", hasher.finalize())",
            "    }",
            "",
            "    pub async fn create_document(",
            "        &self,",
            "        title: &str,",
            "        content: &str,",
            "        tags: &[String],",
            "        metadata: Option<serde_json::Value>,",
            "    ) -> Result<Document, ApiError> {",
            "        let id = Uuid::new_v4().to_string();",
            "        let now = Utc::now().to_rfc3339();",
            "        let tags_json = serde_json::to_string(tags)?;",
            "        let content_hash = Self::compute_hash(content);",
            "        let metadata_json = metadata.map(|m| serde_json::to_string(&m)).transpose()?;",
            "",
            "        sqlx::query(",
            "            \"INSERT INTO documents (id, title, content, content_hash, tags, metadata)\"",
            "        )",
            "        .bind(&id)",
            "        .bind(title)",
            "        .bind(content)",
            "        .bind(&content_hash)",
            "        .bind(&tags_json)",
            "        .bind(&metadata_json)",
            "        .execute(&self.pool)",
            "        .await?;",
            "",
            "        Ok(Document {",
            "            id,",
            "            title: title.to_string(),",
            "            content: content.to_string(),",
            "            content_hash,",
            "            tags: tags.to_vec(),",
            "            metadata,",
            "            created_at: now.clone(),",
            "            updated_at: now,",
            "        })",
            "    }",
            "",
            "    pub async fn get_document(&self, id: &str) -> Result<Option<Document>, ApiError> {",
            "        let row = sqlx::query_as::<_, Document>(",
            "            \"SELECT * FROM documents WHERE id = ?1\"",
            "        )",
            "        .bind(id)",
            "        .fetch_optional(&self.pool)",
            "        .await?;",
            "        Ok(row)",
            "    }",
            "",
            "    pub async fn list_documents(",
            "        &self,",
            "        query: Option<&str>,",
            "        tags: Option<&[String]>,",
            "        limit: Option<i64>,",
            "        offset: Option<i64>,",
            "    ) -> Result<(Vec<Document>, usize), ApiError> {",
            "        let mut sql = String::from(\"SELECT * FROM documents d WHERE 1=1\");",
            "        let mut where_conditions = Vec::new();",
            "        let mut bind_params = Vec::new();",
            "        let mut param_count = 0;",
            "",
            "        if let Some(q) = query {",
            "            param_count += 1;",
            "            where_conditions.push(format!(\"d.id IN (SELECT id FROM documents_fts WHERE documents_fts MATCH ?{})\", param_count));",
            "            bind_params.push(q.to_string());",
            "        }",
            "",
            "        if let Some(tag_list) = tags && !tag_list.is_empty() {",
            "            for tag in tag_list {",
            "                param_count += 1;",
            "                where_conditions.push(format!(\"d.tags LIKE ?{}\", param_count));",
            "                bind_params.push(format!(\"%\\\"{}\\\"%\", tag));",
            "            }",
            "        }",
            "",
            "        let where_clause = where_conditions.join(\" AND \");",
            "    }",
            "}",
        ];

        let mut hashes = std::collections::HashMap::new();
        let mut collisions = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let h = compute_line_hash(line, i + 1);
            if let Some(prev) = hashes.insert(h.clone(), i) {
                collisions += 1;
                eprintln!(
                    "  collision: line {} and {} both hash to {}",
                    prev + 1,
                    i + 1,
                    h
                );
            }
        }

        let collision_rate = collisions as f64 / lines.len() as f64;
        let collision_pct = collision_rate * 100.0;
        assert!(
            collision_rate < 0.15,
            "collision rate {collision_pct:.1}% ({collisions}/{n} lines) is too high for a {n}-line file",
            n = lines.len(),
        );
    }
}
