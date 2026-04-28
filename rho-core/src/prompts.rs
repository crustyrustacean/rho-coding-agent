//! Base identity prompt.
//!
//! The v1 prompt lives at `src/prompts/base.md` and is embedded at compile time.
//! Use [`base_prompt()`] rather than the raw string — the function signature
//! allows runtime substitution to be added later without an API break.

/// The base identity prompt for rho.
///
/// Embedded at compile time from `src/prompts/base.md`. This is the first segment
/// of the system prompt when the user does not pass `--system`.
pub fn base_prompt() -> &'static str {
    include_str!("prompts/base.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_prompt_is_non_empty() {
        assert!(!base_prompt().is_empty(), "base_prompt() must not be empty");
    }

    #[test]
    fn base_prompt_is_valid_utf8() {
        // include_str! guarantees UTF-8 at compile time; this is a runtime sanity check.
        assert!(std::str::from_utf8(base_prompt().as_bytes()).is_ok());
    }
}
