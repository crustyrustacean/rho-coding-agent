//! Shell execution: [`PowerShellExecutor`] and [`RunCommand`] tool.
//!
//! [`PowerShellExecutor`] implements the [`ShellExecutor`] trait from `rho-core`,
//! providing PowerShell-specific command execution with `pwsh`/`powershell`
//! detection, timeout support, and cancellation.
//!
//! [`RunCommand`] is the tool that exposes shell execution to the model. It
//! delegates to a `Box<dyn ShellExecutor>` rather than spawning processes
//! directly, so the tool layer depends on the abstraction.

use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ShellExecutor, ShellOutput, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

// ── PowerShellExecutor ────────────────────────────────────────────────────────

/// A [`ShellExecutor`] that runs commands via PowerShell.
///
/// On construction, detects whether `pwsh` (PowerShell 7+) or `powershell`
/// (Windows PowerShell 5.1) is available. The detected executable is cached
/// for the lifetime of the executor.
///
/// Commands are executed with `-NoProfile -NonInteractive -Command <command>`.
/// When using `powershell.exe`, `-ExecutionPolicy Bypass` is added to avoid
/// script-signing failures.
pub struct PowerShellExecutor {
    /// The detected PowerShell executable (cached at construction time).
    shell: &'static str,
}

impl PowerShellExecutor {
    /// Create a new executor, detecting the available PowerShell.
    ///
    /// Tries `pwsh` first (PowerShell 7+), then falls back to `powershell`
    /// (Windows PowerShell 5.1). Panics if neither is available.
    pub fn new() -> Self {
        let shell = detect_powershell();
        Self { shell }
    }

    /// Returns the detected PowerShell executable name.
    pub fn shell_exe(&self) -> &'static str {
        self.shell
    }

    /// Build the argument list for spawning PowerShell.
    fn build_args(&self, command: &str) -> Vec<String> {
        let mut args = Vec::new();

        // Windows PowerShell needs ExecutionPolicy Bypass to avoid
        // script-signing failures on constrained systems.
        if self.shell == "powershell" {
            args.push("-ExecutionPolicy".to_owned());
            args.push("Bypass".to_owned());
        }

        args.push("-NoProfile".to_owned());
        args.push("-NonInteractive".to_owned());
        args.push("-Command".to_owned());
        args.push(command.to_owned());

        args
    }
}

impl Default for PowerShellExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ShellExecutor for PowerShellExecutor {
    async fn execute(
        &self,
        command: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Result<ShellOutput> {
        if cancel.is_cancelled() {
            return Err(rho_core::RhoError::Unexpected(anyhow::anyhow!(
                "cancelled before execution"
            )));
        }

        let args = self.build_args(command);

        let child = Command::new(self.shell)
            .args(&args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                rho_core::RhoError::Unexpected(anyhow::anyhow!(
                    "shell: failed to spawn `{}`: {e}",
                    self.shell
                ))
            })?;

        let child_id = child.id();

        let run_future = async {
            let output = child.wait_with_output().await.map_err(|e| {
                rho_core::RhoError::Unexpected(anyhow::anyhow!(
                    "shell: failed to wait for process: {e}"
                ))
            })?;

            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit_code = output.status.code().unwrap_or(-1);

            Ok(ShellOutput::new(stdout, stderr, exit_code))
        };

        // Wrap with optional timeout.
        if let Some(dur) = timeout {
            tokio::select! {
                res = tokio::time::timeout(dur, run_future) => {
                    if let Ok(inner) = res { inner } else {
                        // Timeout elapsed — kill the process.
                        kill_process(child_id).await;
                        Err(rho_core::RhoError::Unexpected(anyhow::anyhow!(
                            "shell: command timed out after {}ms",
                            dur.as_millis()
                        )))
                    }
                }
                () = cancel.cancelled() => {
                    kill_process(child_id).await;
                    Err(rho_core::RhoError::Unexpected(anyhow::anyhow!(
                        "cancelled"
                    )))
                }
            }
        } else {
            tokio::select! {
                res = run_future => res,
                () = cancel.cancelled() => {
                    kill_process(child_id).await;
                    Err(rho_core::RhoError::Unexpected(anyhow::anyhow!(
                        "cancelled"
                    )))
                }
            }
        }
    }
}

// ── RunCommand ────────────────────────────────────────────────────────────────

/// Execute a shell command and capture stdout/stderr.
///
/// Delegates to a [`ShellExecutor`] implementation for actual process
/// spawning. The default executor is [`PowerShellExecutor`].
///
/// Runs within the sandbox root as the working directory.
pub struct RunCommand {
    /// Sandbox root used as the working directory for all commands.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> ToolName {
        ToolName::from("run_command")
    }

    fn description(&self) -> &'static str {
        "Execute a PowerShell command within the project directory \
         and return its stdout, stderr, and exit code."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The PowerShell command to execute."
                }
            },
            "required": ["command"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Destructive
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let command = arguments["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("run_command: missing required argument `command`"))?
            .to_owned();

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let shell_output = self
            .executor
            .execute(&command, self.root.path(), None, cancel)
            .await?;

        // Map ShellOutput → ToolResult at the tool boundary.
        let combined = if shell_output.stderr.is_empty() {
            shell_output.stdout.clone()
        } else if shell_output.stdout.is_empty() {
            format!("stderr:\n{}", shell_output.stderr)
        } else {
            format!("{}\nstderr:\n{}", shell_output.stdout, shell_output.stderr)
        };

        let result = if shell_output.is_success() {
            ToolResult::success(format!("exit_code: {}\n{combined}", shell_output.exit_code))
        } else {
            ToolResult::error(format!("exit_code: {}\n{combined}", shell_output.exit_code))
        };

        Ok(ToolOutcome::Immediate(result))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Detect the best available PowerShell executable.
///
/// Tries `pwsh` first (PowerShell 7+), then falls back to `powershell`
/// (Windows PowerShell 5.1). Returns a `&'static str` so the result can be
/// held in the executor without lifetime concerns.
///
/// # Panics
///
/// Panics if neither `pwsh` nor `powershell` is found on `PATH`.
fn detect_powershell() -> &'static str {
    // Try pwsh first.
    if which_exists("pwsh") {
        return "pwsh";
    }
    // Fall back to Windows PowerShell.
    if which_exists("powershell") {
        return "powershell";
    }
    panic!(
        "no PowerShell found on PATH — install PowerShell 7+ (pwsh) \
         or ensure Windows PowerShell (powershell) is available"
    )
}

/// Check whether an executable exists on `PATH`.
fn which_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

/// Kill a process by PID using `taskkill /F /PID <pid>`.
///
/// Used on Windows to forcefully terminate a child process when
/// cancellation or timeout fires. If `child_id` is `None` or the
/// kill fails, the error is silently ignored — we did our best.
async fn kill_process(child_id: Option<u32>) {
    if let Some(id) = child_id {
        let _ = Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/F"])
            .output()
            .await;
    }
}
