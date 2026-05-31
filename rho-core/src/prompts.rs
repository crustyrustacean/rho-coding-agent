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

/// A compact system prompt for models with small context windows.
///
/// Use this when the full [`base_prompt()`] (~2,000 tokens) plus tool schemas
/// and context files would exceed the model's context length. The compact
/// prompt omits PowerShell idioms, pipeline patterns, and Rust-specific
/// guidance, retaining only the essential identity and safety instructions.
pub fn compact_prompt() -> &'static str {
    include_str!("prompts/compact.md")
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

    // ── PowerShell-aware prompt content checks ────────────────────────────
    //
    // These verify that the base prompt contains the key sections and idioms
    // that Task 8 added. If the prompt is restructured, update the assertions
    // but do not remove coverage for a topic without a deliberate decision.

    #[test]
    fn base_prompt_instructs_powershell_not_bash() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("PowerShell"),
            "base prompt must mention PowerShell"
        );
        assert!(
            prompt.contains("never bash"),
            "base prompt must explicitly say never bash"
        );
    }

    #[test]
    fn base_prompt_has_powershell_idioms_table() {
        let prompt = base_prompt();
        // Key idioms that the model must know:
        assert!(
            prompt.contains("Get-ChildItem"),
            "prompt must teach Get-ChildItem idiom"
        );
        assert!(
            prompt.contains("Select-String"),
            "prompt must teach Select-String idiom"
        );
        assert!(
            prompt.contains("Get-Content"),
            "prompt must teach Get-Content idiom"
        );
    }

    #[test]
    fn base_prompt_covers_pipeline_patterns() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("Where-Object"),
            "prompt must cover pipeline filtering"
        );
        assert!(
            prompt.contains("ForEach-Object"),
            "prompt must cover pipeline iteration"
        );
    }

    #[test]
    fn base_prompt_covers_environment_variables() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("$env:"),
            "prompt must show PowerShell env var syntax"
        );
    }

    #[test]
    fn base_prompt_covers_rust_commands() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("cargo check"),
            "prompt must teach cargo check"
        );
        assert!(
            prompt.contains("cargo clippy"),
            "prompt must teach cargo clippy"
        );
        assert!(
            prompt.contains("cargo test"),
            "prompt must teach cargo test"
        );
    }

    #[test]
    fn base_prompt_warns_about_alias_circumvention() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("alias") && prompt.contains("denylist"),
            "prompt must warn against using aliases to bypass denylist"
        );
    }

    #[test]
    fn base_prompt_warns_no_cmd_bypass() {
        let prompt = base_prompt();
        // On Windows, warns against cmd.exe bypass; on other platforms,
        // cmd.exe is not relevant but the warning is harmless.
        assert!(
            prompt.contains("cmd /c") || prompt.contains("cmd.exe"),
            "prompt must warn against using cmd.exe to bypass PowerShell"
        );
    }

    #[test]
    fn base_prompt_covers_path_handling() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("Join-Path") || prompt.contains("Split-Path"),
            "prompt must cover PowerShell path handling"
        );
    }

    #[test]
    fn base_prompt_covers_error_handling() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("ErrorAction"),
            "prompt must cover PowerShell error handling"
        );
    }

    // ── Progress checkpointing checks ─────────────────────────────────────

    #[test]
    fn base_prompt_instructs_checkpointing() {
        let prompt = base_prompt();
        assert!(
            prompt.contains(".rho/checkpoint.md"),
            "prompt must instruct agent to write progress to .rho/checkpoint.md"
        );
        assert!(
            prompt.contains("Progress checkpointing"),
            "prompt must have a progress checkpointing section"
        );
    }

    #[test]
    fn base_prompt_checkpointing_says_before_each_step() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("before starting each step"),
            "prompt must instruct checkpointing before each step, not after"
        );
    }

    #[test]
    fn base_prompt_checkpointing_says_re_read_on_loss() {
        let prompt = base_prompt();
        assert!(
            prompt.contains("re-read") || prompt.contains("re-read `.rho/checkpoint"),
            "prompt must instruct agent to re-read checkpoint when losing context"
        );
    }
}
