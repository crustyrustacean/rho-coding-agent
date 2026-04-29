//! Shell execution tool: [`RunCommand`].
//!
//! PowerShell-first. On Windows `pwsh` is tried first, falling back to
//! `powershell`. Executes within the sandbox root as the working directory.
//!
//! Phase 2 adds command denylist enforcement.

use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use tokio::process::Command;

/// Execute a PowerShell command and capture stdout/stderr.
///
/// Runs within the sandbox root as the working directory.
/// Wires the [`CancellationToken`] to `taskkill` so long-running commands
/// respond to Ctrl-C.
pub struct RunCommand {
    /// Sandbox root used as the working directory for all commands.
    pub root: SandboxRoot,
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

        let shell = powershell_exe();

        let child = Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .current_dir(self.root.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("run_command: failed to spawn `{shell}`: {e}"))?;

        let child_id = child.id();

        let output = tokio::select! {
            result = child.wait_with_output() => {
                result.map_err(|e| anyhow::anyhow!("run_command: failed to wait for process: {e}"))?
            }
            () = cancel.cancelled() => {
                if let Some(id) = child_id {
                    let _ = Command::new("taskkill")
                        .args(["/PID", &id.to_string(), "/F"])
                        .output()
                        .await;
                }
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_code = output.status.code().unwrap_or(-1);

        let combined = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            format!("stderr:\n{stderr}")
        } else {
            format!("{stdout}\nstderr:\n{stderr}")
        };

        let result = if output.status.success() {
            ToolResult::success(format!("exit_code: {exit_code}\n{combined}"))
        } else {
            ToolResult::error(format!("exit_code: {exit_code}\n{combined}"))
        };

        Ok(ToolOutcome::Immediate(result))
    }
}

/// Return the PowerShell executable name.
fn powershell_exe() -> &'static str {
    "pwsh"
}
