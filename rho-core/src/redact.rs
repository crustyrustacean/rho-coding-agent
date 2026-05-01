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
//! # Custom patterns
//!
//! Users can extend the built-in pattern set with custom regex patterns via
//! `.rho/config.toml` (the `[redaction] custom_patterns` field). Invalid regex
//! patterns are silently ignored with a warning to stderr. Custom patterns are
//! applied after built-in patterns.
//!
//! # Disable toggle
//!
//! Redaction can be disabled entirely via `[redaction] enabled = false` in
//! config. This is **not recommended** — the approval gate remains the primary
//! defence, but disabling redaction means secrets may appear in conversation
//! history and be sent to the model API.

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
/// Constructed via [`Redactor::new`] for the standard built-in pattern set, or
/// [`Redactor::from_config`] to include custom regex patterns and respect the
/// enabled toggle from configuration.
///
/// When `enabled` is `false`, [`redact()`] returns the input unchanged.
pub struct Redactor {
    /// Whether redaction is active. When `false`, `redact()` is a no-op.
    enabled: bool,
    /// Compiled custom regex patterns, applied after built-in patterns.
    custom_regexes: Vec<regex::Regex>,
}

impl Default for Redactor {
    fn default() -> Self {
        Self {
            enabled: true,
            custom_regexes: Vec::new(),
        }
    }
}

impl Redactor {
    /// Create a redactor with the built-in pattern set (enabled, no custom patterns).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a redactor from config settings.
    ///
    /// Uses the `enabled` flag and compiles each string in `custom_patterns` as
    /// a regex. Invalid regex patterns are silently skipped with a warning
    /// printed to stderr.
    pub fn from_config(enabled: bool, custom_patterns: &[String]) -> Self {
        let custom_regexes = custom_patterns
            .iter()
            .filter_map(|pat| match regex::Regex::new(pat) {
                Ok(re) => Some(re),
                Err(e) => {
                    eprintln!("Warning: invalid redaction pattern skipped: `{pat}`: {e}");
                    None
                }
            })
            .collect();

        Self {
            enabled,
            custom_regexes,
        }
    }

    /// Returns `true` if this redactor will actually redact text.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Apply all patterns to `text`, returning the redacted version.
    ///
    /// If `enabled` is `false`, returns the input unchanged.
    ///
    /// Otherwise runs in a single left-to-right pass per built-in pattern,
    /// then applies each custom regex pattern. A match by an earlier pattern
    /// is never re-scanned by a later one (because `[REDACTED]` itself does
    /// not match any built-in prefix or typical custom pattern).
    pub fn redact(&self, text: &str) -> String {
        if !self.enabled {
            return text.to_owned();
        }

        let mut current = text.to_owned();
        for pattern in built_in_patterns() {
            current = redact_pattern(&current, &pattern);
        }
        for re in &self.custom_regexes {
            current = re.replace_all(&current, REDACTED).into_owned();
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

    // ── Enabled toggle ────────────────────────────────────────────────────

    #[test]
    fn disabled_redactor_returns_input_unchanged() {
        let r = Redactor::from_config(false, &[]);
        let key = "sk-".to_owned() + &"x".repeat(32);
        let text = format!("found key: {key}");
        assert_eq!(r.redact(&text), text);
    }

    #[test]
    fn enabled_redactor_redacts_normally() {
        let r = Redactor::from_config(true, &[]);
        let key = "sk-".to_owned() + &"x".repeat(32);
        let text = format!("found key: {key}");
        assert!(r.redact(&text).contains(REDACTED));
    }

    #[test]
    fn is_enabled_reflects_state() {
        assert!(Redactor::from_config(true, &[]).is_enabled());
        assert!(!Redactor::from_config(false, &[]).is_enabled());
        assert!(Redactor::new().is_enabled());
    }

    // ── Custom patterns ───────────────────────────────────────────────────

    #[test]
    fn custom_regex_pattern_redacts() {
        let r = Redactor::from_config(true, &[r"my-key-[a-zA-Z0-9]{16}".to_owned()]);
        let text = "key=my-key-abcdefghijklmnop";
        let out = r.redact(text);
        assert!(out.contains(REDACTED), "expected REDACTED in: {out}");
        assert!(!out.contains("my-key-abcdefghijklmnop"));
    }

    #[test]
    fn custom_pattern_applied_after_builtin() {
        // Built-in patterns run first; custom patterns run after.
        // A custom pattern can catch things the built-ins miss.
        let r = Redactor::from_config(true, &[r"COMPANY_TOKEN_\S+".to_owned()]);
        let text = "COMPANY_TOKEN_abc123 xyz";
        let out = r.redact(text);
        assert!(out.contains(REDACTED), "expected REDACTED in: {out}");
        assert!(!out.contains("COMPANY_TOKEN_abc123"));
    }

    #[test]
    fn custom_pattern_and_builtin_both_match() {
        let r = Redactor::from_config(true, &[r"custom-\S+".to_owned()]);
        let sk_key = "sk-".to_owned() + &"x".repeat(32);
        let text = format!("{sk_key} and custom-secret");
        let out = r.redact(&text);
        assert_eq!(out.matches(REDACTED).count(), 2);
    }

    #[test]
    fn invalid_custom_pattern_is_skipped() {
        // An invalid regex should be silently skipped, not panic.
        let r = Redactor::from_config(true, &[r"[invalid(".to_owned()]);
        // The redactor should still work with built-in patterns.
        let key = "sk-".to_owned() + &"x".repeat(32);
        let text = format!("found key: {key}");
        assert!(r.redact(&text).contains(REDACTED));
    }

    #[test]
    fn no_custom_patterns_is_same_as_new() {
        let r1 = Redactor::new();
        let r2 = Redactor::from_config(true, &[]);
        let text = "sk-".to_owned() + &"x".repeat(32);
        assert_eq!(r1.redact(&text), r2.redact(&text));
    }

    #[test]
    fn multiple_custom_patterns() {
        let r = Redactor::from_config(
            true,
            &[
                r"COMPANY_KEY_\S+".to_owned(),
                r"secret-token-\d+".to_owned(),
            ],
        );
        let text = "COMPANY_KEY_abc and secret-token-123";
        let out = r.redact(text);
        assert_eq!(out.matches(REDACTED).count(), 2);
    }
}
