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
/// 2. Parses the NDJSON output into [`Diagnostic`](rho_core::Diagnostic) values.
/// 3. Filters out dependency noise (diagnostics from outside the workspace).
/// 4. Returns a human-readable summary for the model, with structured
///    [`Diagnostic`](rho_core::Diagnostic) data in [`ToolResultDetails::Diagnostics`].
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

/// Run `cargo test -- --format json` and return structured test results.
///
/// Parses JSON test output (each line is a JSON object with fields:
/// `name`, `event`, `duration_ms`, `stdout`, `stderr`) and returns a clear summary
/// showing passed/failed/ignored counts with details for failures.
///
/// ## Optional parameters
///
/// - `package` — restrict tests to a single workspace member (passed as
///   `--package <name>`). Omit to test the entire workspace.
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

        // Build the command. `--package` is a cargo flag (before `--`); a
        // test-name filter is a libtest positional (after `--`). We do NOT use
        // `--format json` — it is unstable, and on stable Rust the test
        // harness rejects it (`The "json" format is only accepted on the
        // nightly compiler`), which made this tool report an empty
        // "Tests failed:" with no detail every time. Parse the stable
        // human-readable output instead.
        let mut cmd = String::from("cargo test");
        if let Some(pkg) = package {
            cmd.push_str(" --package ");
            cmd.push_str(pkg);
        }
        if let Some(test) = test_name {
            cmd.push_str(" -- ");
            cmd.push_str(test);
        }

        let shell_output = self
            .executor
            .execute(&cmd, self.root.path(), None, cancel, None)
            .await?;

        let results = parse_test_results(&shell_output.stdout);
        let has_results = results.passed > 0
            || results.failed > 0
            || results.ignored > 0
            || !results.failed_names.is_empty();
        let has_failures = results.failed > 0 || shell_output.exit_code != 0;

        if !has_results {
            // No test output at all.
            if shell_output.exit_code == 0 {
                return Ok(ToolOutcome::Immediate(ToolResult::success(
                    "no tests to run".to_owned(),
                )));
            }
            // Non-zero exit with no test output — cargo itself failed
            // (compile error, bad flags, the stable-Rust `--format json`
            // rejection, etc.). Surface stdout/stderr so the caller can see
            // why, instead of the old opaque empty `"Tests failed:\n"`.
            let detail = tool_output_tail(&shell_output.stdout, &shell_output.stderr);
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "cargo test failed (exit {}){detail}",
                shell_output.exit_code
            ))));
        }

        let headline = format!(
            "Test results: {} passed, {} failed, {} ignored",
            results.passed, results.failed, results.ignored
        );

        let outcome = if has_failures {
            let mut output = headline;
            output.push('\n');
            if !results.failed_names.is_empty() {
                output.push_str("\nFailed tests:\n");
                for name in &results.failed_names {
                    output.push_str("  - ");
                    output.push_str(name);
                    output.push('\n');
                }
            }
            output.push_str(&tool_output_tail(
                &shell_output.stdout,
                &shell_output.stderr,
            ));
            ToolOutcome::Immediate(ToolResult::error(output))
        } else {
            ToolOutcome::Immediate(ToolResult::success(headline))
        };

        Ok(outcome)
    }
}

// ── cargo test output parsing ────────────────────────────────────────────────

/// Parsed counts from `cargo test`'s human-readable output.
struct TestResults {
    /// Total tests that passed (summed across all test binaries).
    passed: u32,
    /// Total tests that failed.
    failed: u32,
    /// Total tests ignored.
    ignored: u32,
    /// Names of failing tests, in encounter order.
    failed_names: Vec<String>,
}

/// Parse `cargo test`'s stable human-readable output: sums the `test result:`
/// summary lines across all test binaries and collects failing test names.
fn parse_test_results(stdout: &str) -> TestResults {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut ignored = 0u32;
    let mut failed_names = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(summary) = trimmed.strip_prefix("test result: ") {
            passed += parse_count(summary, "passed");
            failed += parse_count(summary, "failed");
            ignored += parse_count(summary, "ignored");
        } else if let Some(name) = parse_failed_test_name(trimmed) {
            failed_names.push(name);
        }
    }

    TestResults {
        passed,
        failed,
        ignored,
        failed_names,
    }
}

/// Extract the integer preceding `label` from a `test result:` summary line.
/// e.g. on `"ok. 68 passed; 2 failed; 0 ignored"`, `parse_count(_, "failed")`
/// returns `2`.
fn parse_count(summary: &str, label: &str) -> u32 {
    let suffix = format!(" {label}");
    for part in summary.split(';') {
        let part = part.trim();
        if let Some(n) = part
            .strip_suffix(&suffix)
            .and_then(|before| before.split_whitespace().next_back())
            .and_then(|n| n.parse::<u32>().ok())
        {
            return n;
        }
    }
    0
}

/// From a line like `test module::test_name ... FAILED`, return the test name.
fn parse_failed_test_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("test ")?;
    let (name, result) = rest.split_once(" ... ")?;
    result.starts_with("FAILED").then(|| name.to_owned())
}

/// A bounded tail of the test output for failure context: failing-test panics
/// live near the end of stdout; compile errors live in stderr. Both are
/// truncated so the tool result stays a manageable size.
fn tool_output_tail(stdout: &str, stderr: &str) -> String {
    const STDOUT_MAX: usize = 4000;
    const STDERR_MAX: usize = 2000;
    let mut out = String::new();
    if !stderr.trim().is_empty() {
        out.push_str("\nstderr:\n");
        out.push_str(&tail_chars(stderr, STDERR_MAX));
    }
    if !stdout.trim().is_empty() {
        out.push_str("\noutput (tail):\n");
        out.push_str(&tail_chars(stdout, STDOUT_MAX));
    }
    out
}

/// The last `max_chars` of `s` at a UTF-8 boundary, prefixed if truncated.
fn tail_chars(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_owned();
    }
    let mut start = s.len().saturating_sub(max_chars);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    let mut out = String::from("... [truncated] ...\n");
    out.push_str(&s[start..]);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_count_extracts_totals_from_summary_line() {
        let s = "ok. 68 passed; 3 failed; 1 ignored; 0 measured; 0 filtered out";
        assert_eq!(parse_count(s, "passed"), 68);
        assert_eq!(parse_count(s, "failed"), 3);
        assert_eq!(parse_count(s, "ignored"), 1);
        // "filtered out" must not be mistaken for "failed".
        assert_eq!(parse_count(s, "measured"), 0);
    }

    #[test]
    fn parse_failed_test_name_extracts_qualified_name() {
        assert_eq!(
            parse_failed_test_name(
                "test session::persist::tests::list_sessions_skips_non_jsonl_files ... FAILED",
            ),
            Some("session::persist::tests::list_sessions_skips_non_jsonl_files".to_owned())
        );
        // Non-failure lines are ignored.
        assert_eq!(parse_failed_test_name("test foo ... ok"), None);
        assert_eq!(
            parse_failed_test_name("test result: FAILED. 1 passed; 2 failed"),
            None
        );
    }

    #[test]
    fn parse_test_results_sums_across_binaries_and_collects_failures() {
        let stdout = "\
test a::t1 ... ok
test a::t2 ... FAILED
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured

test b::t3 ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured
";
        let r = parse_test_results(stdout);
        assert_eq!(r.passed, 2);
        assert_eq!(r.failed, 1);
        assert_eq!(r.ignored, 0);
        assert_eq!(r.failed_names, vec!["a::t2".to_owned()]);
    }
}
