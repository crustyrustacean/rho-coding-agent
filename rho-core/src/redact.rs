//! Secret redaction.
//!
//! [`Redactor`] scans tool output for known secret patterns and replaces them
//! with `[REDACTED]` before the text enters conversation history.
//!
//!
//! # Best-effort limitation
//!
//! Redaction catches recognisable prefix-shaped secrets and misses everything
//! else. Do not rely on it as a security guarantee.
//!
//! [`Redactor`]: crate::redact::Redactor
//!
//! # Limitation
//!
//! Redaction is **best-effort**. It catches recognisable prefix-shaped secrets
//! (`OpenAI` keys, GitHub PATs, Slack tokens, AWS access key IDs, bearer tokens)
//! and misses everything else — high-entropy strings without a recognisable
//! prefix, internal API keys with custom formats, and secrets the model itself
//! generates. The approval gate is the primary defence; redaction reduces
//! accidental exposure, it does not eliminate it.
//!
//! Users can extend the pattern set or disable redaction in config (Phase 2).

/// Replacement text for any matched secret.
const REDACTED: &str = "[REDACTED]";

// ── Pattern definitions ───────────────────────────────────────────────────────

/// A secret pattern: a fixed prefix followed by a body of known character class
/// and length constraints.
struct Pattern {
    /// Literal prefix that must match exactly (case-sensitive).
    prefix: &'static str,
    /// Returns `true` for characters that form the body.
    body_char: fn(char) -> bool,
    /// Minimum number of body characters required.
    min_body: usize,
    /// Maximum number of body characters to consume (caps the match).
    max_body: usize,
}

/// All built-in patterns, in evaluation order.
fn built_in_patterns() -> [Pattern; 5] {
    [
        // OpenAI API key: sk-... or sk-proj-... (modern format)
        Pattern {
            prefix: "sk-",
            body_char: |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_',
            min_body: 20,
            max_body: 128,
        },
        // GitHub personal access token
        Pattern {
            prefix: "ghp_",
            body_char: |c: char| c.is_ascii_alphanumeric(),
            min_body: 36,
            max_body: 40,
        },
        // Slack tokens: xoxb-, xoxp-, xoxa-, xoxs-
        Pattern {
            prefix: "xox",
            body_char: |c: char| c.is_ascii_alphanumeric() || c == '-',
            min_body: 10,
            max_body: 128,
        },
        // AWS access key ID
        Pattern {
            prefix: "AKIA",
            body_char: |c: char| c.is_ascii_uppercase() || c.is_ascii_digit(),
            min_body: 16,
            max_body: 16,
        },
        // Bearer tokens in HTTP-style headers
        Pattern {
            prefix: "Bearer ",
            body_char: |c: char| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/')
            },
            min_body: 8,
            max_body: 512,
        },
    ]
}

// ── Redactor ──────────────────────────────────────────────────────────────────

/// Scans text for known secret patterns and replaces them with `[REDACTED]`.
///
/// Constructed via [`Redactor::default`] for the standard built-in pattern set.
/// Custom patterns are added in Phase 2 via config.
#[derive(Default)]
pub struct Redactor;

impl Redactor {
    /// Create a redactor with the built-in pattern set.
    pub fn new() -> Self {
        Self
    }

    /// Apply all patterns to `text`, returning the redacted version.
    ///
    /// Runs in a single left-to-right pass per pattern, with patterns applied
    /// sequentially. A match by an earlier pattern is never re-scanned by a
    /// later one (because `[REDACTED]` itself does not match any prefix).
    pub fn redact(&self, text: &str) -> String {
        let mut current = text.to_owned();
        for pattern in built_in_patterns() {
            current = redact_pattern(&current, &pattern);
        }
        current
    }
}

/// Apply one pattern to the input, returning the redacted string.
fn redact_pattern(text: &str, pattern: &Pattern) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while !remaining.is_empty() {
        if let Some(idx) = remaining.find(pattern.prefix) {
            // Append everything before the prefix as-is.
            result.push_str(&remaining[..idx]);
            let after_prefix = &remaining[idx + pattern.prefix.len()..];

            // Count consecutive body characters.
            let body_len = after_prefix
                .chars()
                .take(pattern.max_body)
                .take_while(|&c| (pattern.body_char)(c))
                .map(char::len_utf8)
                .sum::<usize>();

            if body_len >= pattern.min_body {
                // Match: emit prefix and body as REDACTED.
                result.push_str(REDACTED);
                remaining = &after_prefix[body_len..];
            } else {
                // Not enough body chars — not a match. Emit the prefix literally.
                result.push_str(pattern.prefix);
                remaining = after_prefix;
            }
        } else {
            // No more occurrences of the prefix.
            result.push_str(remaining);
            break;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key() {
        let r = Redactor::new();
        let text = "key=sk-abcdefghijklmnopqrstuvwxyz12345 done";
        let out = r.redact(text);
        assert!(out.contains(REDACTED), "expected REDACTED in: {out}");
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz12345"));
    }

    #[test]
    fn redacts_github_pat() {
        let r = Redactor::new();
        let token = "ghp_".to_owned() + &"a".repeat(36);
        let text = format!("token={token}");
        let out = r.redact(&text);
        assert!(out.contains(REDACTED));
        assert!(!out.contains(&token));
    }

    #[test]
    fn redacts_slack_token() {
        let r = Redactor::new();
        let text = "SLACK_BOT_TOKEN=xoxb-12345-abcdefghijklmno extra";
        let out = r.redact(text);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("xoxb-12345-abcdefghijklmno"));
    }

    #[test]
    fn redacts_aws_access_key() {
        let r = Redactor::new();
        let text = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE ok";
        let out = r.redact(text);
        assert!(out.contains(REDACTED));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn redacts_bearer_token() {
        let r = Redactor::new();
        let text = "Authorization: Bearer eyJhbGciOiJSUzI1NiJ9.payload.sig rest";
        let out = r.redact(text);
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn does_not_redact_short_sk_prefix() {
        // "sk-" with fewer than min_body chars should not be redacted.
        let r = Redactor::new();
        let text = "sk-short";
        let out = r.redact(text);
        assert_eq!(out, text);
    }

    #[test]
    fn preserves_surrounding_text() {
        let r = Redactor::new();
        let key = "sk-".to_owned() + &"x".repeat(32);
        let text = format!("before {key} after");
        let out = r.redact(&text);
        assert!(out.starts_with("before "));
        assert!(out.ends_with(" after"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn multiple_secrets_in_one_string() {
        let r = Redactor::new();
        let k1 = "sk-".to_owned() + &"a".repeat(30);
        let k2 = "AKIAIOSFODNN7EXAMPLE";
        let text = format!("{k1} and {k2}");
        let out = r.redact(&text);
        assert_eq!(out.matches(REDACTED).count(), 2);
    }

    #[test]
    fn empty_input_returns_empty() {
        let r = Redactor::new();
        assert_eq!(r.redact(""), "");
    }
}
