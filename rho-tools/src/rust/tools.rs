//! Cargo tool implementations: [`CargoCheck`], [`CargoClippy`], [`CargoTest`], [`CargoFix`], [`RustcExplain`].

use async_trait::async_trait;
use rho_core::diagnostic::DiagnosticLevel;
use rho_core::tool::{CancellationToken, Tool, ToolOutcome, ToolResult, ToolResultDetails};
use rho_core::{Result, SandboxRoot, ShellExecutor, ToolName, ToolRisk};
use serde_json;

use crate::error::ToolError;

use super::format::format_diagnostics_for_model;
use super::parse::parse_cargo_diagnostics;

// ── Shared execution helper ───────────────────────────────────────────────────

/// Execute a cargo diagnostic tool and return appropriate tool outcome.
///
/// This helper centralizes the common logic for cargo check, clippy, and test:
/// run the command, parse diagnostics, and return either an error or a
/// structured result with diagnostics.
pub(super) fn execute_cargo_diagnostic_tool(
    tool_name: &str,
    ndjson_output: &str,
    workspace_root: &std::path::Path,
) -> ToolOutcome {
    let diagnostics = parse_cargo_diagnostics(ndjson_output, workspace_root);

    let has_errors = diagnostics
        .iter()
        .any(|d| matches!(d.level, DiagnosticLevel::Error));

    if diagnostics.is_empty() {
        return ToolOutcome::Immediate(ToolResult::success(format_diagnostics_for_model(
            tool_name,
            &diagnostics,
        )));
    }

    let output = format_diagnostics_for_model(tool_name, &diagnostics);
    ToolOutcome::Immediate(ToolResult {
        output,
        is_error: has_errors,
        details: ToolResultDetails::Diagnostics(diagnostics),
    })
}

// ── CargoCheck tool ───────────────────────────────────────────────────────────

/// Run `cargo check --message-format=json` and return structured diagnostics.
///
/// The tool:
/// 1. Runs `cargo check --message-format=json` via the configured
///    [`ShellExecutor`] within the sandbox root.
/// 2. Parses the NDJSON output into [`Diagnostic`] values.
/// 3. Filters out dependency noise (diagnostics from outside the workspace).
/// 4. Returns a human-readable summary for the model, with structured
///    [`Diagnostic`] data in [`ToolResultDetails::Diagnostics`].
///
/// ## Optional parameters
///
/// - `package` — restrict check to a single workspace member (passed as
///   `--package <name>`). Omit to check the entire workspace.
pub struct CargoCheck {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoCheck {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_check")
    }

    fn description(&self) -> &str {
        "Run `cargo check` and return structured compiler diagnostics. \
         Returns error codes, messages, file locations, and machine-applicable \
         fix suggestions. Use this before attempting to fix compilation errors."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: check only this workspace member package name."
                }
            },
            "required": []
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo check --message-format=json");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        Ok(execute_cargo_diagnostic_tool(
            "cargo check",
            &shell_output.stdout,
            self.root.path(),
        ))
    }
}

// ── CargoClippy tool ──────────────────────────────────────────────────────────

/// Run `cargo clippy --message-format=json` and return structured diagnostics.
///
/// Similar to [`CargoCheck`] but runs clippy for additional linting.
///
/// ## Optional parameters
///
/// - `package` — restrict clippy to a single workspace member.
pub struct CargoClippy {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoClippy {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_clippy")
    }

    fn description(&self) -> &str {
        "Run `cargo clippy` and return structured lint diagnostics. \
         Returns error codes, messages, file locations, and machine-applicable \
         fix suggestions. Use this to catch code quality issues beyond \
         what cargo check reports."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: check only this workspace member package name."
                }
            },
            "required": []
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo clippy --message-format=json");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        Ok(execute_cargo_diagnostic_tool(
            "cargo clippy",
            &shell_output.stdout,
            self.root.path(),
        ))
    }
}

// ── RustcExplain tool ─────────────────────────────────────────────────────────

/// Run `rustc --explain <error-code>` and return the compiler's explanation.
///
/// Provides detailed explanations for specific Rust error codes.
///
/// ## Required parameters
///
/// - `error_code` — the error code to explain (e.g., `"E0308"`).
pub struct RustcExplain {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for RustcExplain {
    fn name(&self) -> ToolName {
        ToolName::from("rustc_explain")
    }

    fn description(&self) -> &str {
        "Run `rustc --explain <error-code>` and return the compiler's explanation. \
         Provides detailed explanations and examples for specific Rust error codes. \
         Use this to understand compilation errors that cargo check reports."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "error_code": {
                    "type": "string",
                    "description": "The error code to explain (e.g., \"E0308\")."
                }
            },
            "required": ["error_code"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let error_code = arguments
            .get("error_code")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::MissingArgument {
                name: "error_code".to_string(),
            })?;

        let cmd = format!("rustc --explain {error_code}");

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        Ok(ToolOutcome::Immediate(ToolResult::success(
            shell_output.stdout,
        )))
    }
}

// ── CargoTest tool ───────────────────────────────────────────────────────────

/// Run `cargo test` and return test results.
///
/// Runs tests and reports failures with structured output.
///
/// ## Optional parameters
///
/// - `package` — restrict tests to a single workspace member.
/// - `test_name` — run only the specified test.
pub struct CargoTest {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoTest {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_test")
    }

    fn description(&self) -> &str {
        "Run `cargo test` and return test results. \
         Reports test failures with output for debugging. Use this to verify \
         that fixes don't break existing functionality."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: run tests only for this workspace member package name."
                },
                "test_name": {
                    "type": "string",
                    "description": "Optional: run only this specific test name."
                }
            },
            "required": []
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);
        let test_name = arguments
            .get("test_name")
            .and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo test");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }
        if let Some(test) = test_name {
            cmd.push(' ');
            cmd.push_str(test);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        if shell_output.exit_code == 0 {
            Ok(ToolOutcome::Immediate(ToolResult::success(
                "All tests passed".to_string(),
            )))
        } else {
            Ok(ToolOutcome::Immediate(ToolResult::success(format!(
                "Tests failed:\n{}",
                shell_output.stdout
            ))))
        }
    }
}

// ── CargoFix tool ────────────────────────────────────────────────────────────

/// Run `cargo fix --allow-dirty --allow-staged` and apply machine-applicable fixes.
///
/// Applies machine-applicable suggestions from cargo check/clippy.
///
/// ## Optional parameters
///
/// - `package` — restrict fixes to a single workspace member.
pub struct CargoFix {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// The shell executor that runs commands.
    pub executor: Box<dyn ShellExecutor>,
}

#[async_trait]
impl Tool for CargoFix {
    fn name(&self) -> ToolName {
        ToolName::from("cargo_fix")
    }

    fn description(&self) -> &str {
        "Run `cargo fix --allow-dirty --allow-staged` and apply machine-applicable fixes. \
         Applies compiler-suggested fixes automatically. Use this after running \
         cargo check or clippy to apply available fixes. This is a destructive \
         operation that modifies source files."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Optional: fix only this workspace member package name."
                }
            },
            "required": []
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let package = arguments.get("package").and_then(serde_json::Value::as_str);

        let mut cmd = String::from("cargo fix --allow-dirty --allow-staged");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        let mut result = String::from("cargo fix completed");
        if !shell_output.stdout.is_empty() {
            result.push_str("\n\nOutput:\n");
            result.push_str(&shell_output.stdout);
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(result)))
    }
}
