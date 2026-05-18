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
    pub fn new(stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: stderr.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
