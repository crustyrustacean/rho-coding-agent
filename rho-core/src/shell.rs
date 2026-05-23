//! Shell execution abstraction.
//!
//! [`ShellExecutor`] defines the interface for running commands in a shell.
//! The trait lives in `rho-core` so the tool layer depends on the abstraction,
//! not on any concrete shell. The first implementation ([`PowerShellExecutor`])
//! lives in `rho-tools`.
//!
//! [`PowerShellExecutor`]: rho_tools::PowerShellExecutor

use crate::error::Result;
use crate::tool::CancellationToken;
use async_trait::async_trait;
use std::path::Path;
use std::time::Duration;

// ── ShellOutput ───────────────────────────────────────────────────────────────

/// Output captured from a shell command execution.
///
/// Preserves the structured `stdout`/`stderr`/`exit_code` separation that tools
/// like `RunCommand` need for presentation. The tool maps `ShellOutput → ToolResult`
/// at the boundary — it decides how to format the combined output string and
/// what counts as an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellOutput {
    /// The command's standard output.
    pub stdout: String,
    /// The command's standard error.
    pub stderr: String,
    /// The process exit code (0 = success).
    pub exit_code: i32,
}

impl ShellOutput {
    /// Create a new `ShellOutput`.
    ///
    /// ANSI escape sequences (e.g. colour codes from `cargo`) are stripped
    /// from both `stdout` and `stderr` so that tool results render as plain
    /// text in the REPL instead of injecting terminal colour changes.
    pub fn new(stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) -> Self {
        Self {
            stdout: strip_ansi(&stdout.into()),
            stderr: strip_ansi(&stderr.into()),
            exit_code,
        }
    }

    /// Returns `true` if the command exited successfully (exit code 0).
    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }
}

// ── ShellExecutor ─────────────────────────────────────────────────────────────

/// The interface for executing shell commands.
///
/// Implementations target specific shells (PowerShell, Bash, etc.).
/// The trait lives in `rho-core` so the tool layer depends on the
/// abstraction, not on any concrete shell.
///
/// # Dyn-compatibility
///
/// `#[async_trait]` is required because tools store `Box<dyn ShellExecutor>`.
#[async_trait]
pub trait ShellExecutor: Send + Sync {
    /// Execute `command` in `working_dir` and return the captured output.
    ///
    /// If `timeout` is `Some(duration)`, the process is killed if it
    /// exceeds the deadline and an error result is returned.
    ///
    /// `cancel` is checked at spawn time and during the wait; if cancelled,
    /// the child process is killed and an error result is returned.
    ///
    /// `input` is an optional string piped to the child's stdin. When
    /// `None`, stdin is closed immediately (EOF) so that interactive
    /// prompts fail fast instead of hanging.
    async fn execute(
        &self,
        command: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
        cancel: CancellationToken,
        input: Option<&str>,
    ) -> Result<ShellOutput>;
}

/// Strip ANSI escape sequences from a string.
///
/// Handles the common `ESC [ <params> <letter>` pattern used by terminals
/// for colour, cursor movement, etc.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Consume parameter bytes (0x30–0x3f: digits and semicolons).
                while let Some(&c) = chars.peek() {
                    if ('0'..='?').contains(&c) || c == ';' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Consume intermediate bytes (0x20–0x2f).
                while let Some(&c) = chars.peek() {
                    if (' '..='/').contains(&c) {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Consume the final byte (0x40–0x7e).
                if chars.peek().is_some_and(|c| ('@'..'~').contains(c)) {
                    chars.next();
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        let input = "\x1b[31;1merror\x1b[0m: something failed";
        assert_eq!(strip_ansi(input), "error: something failed");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn strip_ansi_handles_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_handles_nested_codes() {
        let input = "\x1b[1m\x1b[31mred bold\x1b[0m normal";
        assert_eq!(strip_ansi(input), "red bold normal");
    }

    #[test]
    fn shell_output_strips_ansi_on_construction() {
        let out = ShellOutput::new("\x1b[32mok\x1b[0m", "\x1b[31;1merr\x1b[0m", 0);
        assert_eq!(out.stdout, "ok");
        assert_eq!(out.stderr, "err");
    }

    #[test]
    fn shell_output_new_constructs_fields() {
        let out = ShellOutput::new("hello", "err", 0);
        assert_eq!(out.stdout, "hello");
        assert_eq!(out.stderr, "err");
        assert_eq!(out.exit_code, 0);
    }

    #[test]
    fn shell_output_is_success_true_for_zero() {
        let out = ShellOutput::new("", "", 0);
        assert!(out.is_success());
    }

    #[test]
    fn shell_output_is_success_false_for_nonzero() {
        let out = ShellOutput::new("", "", 1);
        assert!(!out.is_success());
    }

    #[test]
    fn shell_output_is_success_false_for_negative() {
        let out = ShellOutput::new("", "", -1);
        assert!(!out.is_success());
    }

    #[test]
    fn shell_output_equality() {
        let a = ShellOutput::new("out", "err", 42);
        let b = ShellOutput::new("out", "err", 42);
        assert_eq!(a, b);
    }

    #[test]
    fn shell_output_inequality_different_stdout() {
        let a = ShellOutput::new("a", "err", 0);
        let b = ShellOutput::new("b", "err", 0);
        assert_ne!(a, b);
    }
}
