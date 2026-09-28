//! `wait_for` — block on external state without burning agent iterations.
//!
//! Polling from the model costs one iteration per poll. `wait_for` polls
//! *inside* the tool and returns once, so a five-minute wait is one turn.

use crate::ToolError;
use crate::shell::CommandDenylist;
use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ShellExecutor, ShellOutput, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use serde_json::json;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Default total wait budget when `timeout_secs` is omitted.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Lower clamp on `timeout_secs`.
const MIN_TIMEOUT_SECS: u64 = 1;
/// Upper clamp on `timeout_secs` — a day is far past any useful wait.
const MAX_TIMEOUT_SECS: u64 = 86_400;
/// Default interval between polls when `poll_secs` is omitted.
const DEFAULT_POLL_SECS: u64 = 10;
/// Lower clamp on `poll_secs`.
const MIN_POLL_SECS: u64 = 1;
/// Upper clamp on `poll_secs`.
const MAX_POLL_SECS: u64 = 300;

/// What makes a poll "satisfied" and end the wait.
#[derive(Clone, Debug, PartialEq)]
enum Until {
    /// Exit code equals the given value. The default (`0`).
    ExitCode(i32),
    /// Exit code is zero.
    Success,
    /// Output matches the pattern (case-insensitive substring).
    Regex(String),
}

impl Until {
    /// Whether this poll result satisfies the predicate.
    fn satisfied(&self, out: &ShellOutput) -> bool {
        match self {
            Self::ExitCode(code) => out.exit_code == *code,
            Self::Success => out.exit_code == 0,
            Self::Regex(pattern) => {
                let haystack = format!("{}{}", out.stdout, out.stderr).to_lowercase();
                haystack.contains(&pattern.to_lowercase())
            }
        }
    }
}

/// Block until a command's result satisfies a predicate, or the budget expires.
///
/// Wraps a probe command and re-runs it on an interval, entirely inside the
/// tool. The agent loop sees one tool result instead of one per poll.
pub struct WaitFor {
    /// Sandbox root used as the working directory.
    pub root: SandboxRoot,
    /// Shell executor that runs the probe.
    pub executor: Box<dyn ShellExecutor>,
    /// Denylist to check before execution.
    pub denylist: CommandDenylist,
}

impl WaitFor {
    /// Render a poll result plus a trailing status line.
    fn render(out: &ShellOutput, polls: u32, elapsed: Duration) -> String {
        let secs = elapsed.as_secs_f64();
        let mut text = String::new();
        if !out.stdout.is_empty() {
            text.push_str(out.stdout.trim_end());
            text.push('\n');
        }
        if !out.stderr.is_empty() {
            text.push_str(out.stderr.trim_end());
            text.push('\n');
        }
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "[wait_for: {polls} poll{}, {secs:.0}s elapsed, exit {}]",
                if polls == 1 { "" } else { "s" },
                out.exit_code
            ),
        );
        text
    }
}

#[async_trait]
impl Tool for WaitFor {
    fn name(&self) -> ToolName {
        ToolName::from("wait_for")
    }

    fn description(&self) -> &str {
        "Block until a command's result satisfies a condition, then return. \
         Use this instead of sleeping in run_command and polling yourself: \
         waiting from the model costs one turn per poll, whereas this polls \
         internally and costs one turn for the whole wait. \
         \
         Canonical use — wait for a CI run: \
         wait_for { command: \"gh run watch <id> --exit-status\", timeout_secs: 600 }. \
         \
         Also useful for waiting on a file to appear, a dev server to come up, \
         a port to open, or a lock to clear."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The probe command to run on each poll. Its result is matched against `until`."
                },
                "until": {
                    "type": "string",
                    "description": "When the wait is satisfied. \"success\" (default) means exit code 0. \"exit_code: N\" matches that exact code. \"regex: PATTERN\" matches case-insensitively anywhere in stdout or stderr."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!("Total budget for the wait, in seconds. Default {DEFAULT_TIMEOUT_SECS}.")
                },
                "poll_secs": {
                    "type": "integer",
                    "description": format!("Seconds between polls. Default {DEFAULT_POLL_SECS}.")
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

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // Denylist before the loop. The command is constant across polls, so one
        // check is sufficient — and skipping it would make this a bypass.
        if let Some(reason) = self.denylist.check(&command) {
            warn!(command = %command, reason = %reason, "command denied by denylist");
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                "command denied: {reason}"
            ))));
        }

        let until = parse_until(arguments["until"].as_str());
        let timeout = clamp(
            arguments["timeout_secs"].as_u64(),
            DEFAULT_TIMEOUT_SECS,
            MIN_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
        );
        let mut poll = clamp(
            arguments["poll_secs"].as_u64(),
            DEFAULT_POLL_SECS,
            MIN_POLL_SECS,
            MAX_POLL_SECS,
        );
        // A poll interval longer than the whole budget would make the first
        // timeout check the only one — never longer.
        poll = poll.min(timeout);

        let working_dir = self.root.path().to_path_buf();
        let start = Instant::now();
        let mut polls: u32 = 0;

        loop {
            let remaining = timeout.saturating_sub(start.elapsed().as_secs());
            // Bound each poll independently: a wedged probe must not consume the
            // entire budget in one shot.
            let poll_timeout = Duration::from_secs(poll.saturating_mul(2))
                .min(Duration::from_secs(remaining.max(1)));

            polls += 1;
            debug!(command = %command, poll = polls, "wait_for: polling");
            let out = self
                .executor
                .execute(
                    &command,
                    &working_dir,
                    Some(poll_timeout),
                    cancel.clone(),
                    None,
                )
                .await?;

            if until.satisfied(&out) {
                info!(command = %command, polls, "wait_for: condition satisfied");
                return Ok(ToolOutcome::Immediate(ToolResult::success(Self::render(
                    &out,
                    polls,
                    start.elapsed(),
                ))));
            }

            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error(
                    "wait_for: cancelled while waiting",
                )));
            }

            let elapsed = start.elapsed();
            if elapsed.as_secs() >= timeout {
                info!(command = %command, polls, "wait_for: timed out");
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!(
                    "{}\n[wait_for: timed out after {:.0}s across {polls} poll{} \
                     without satisfying the condition]",
                    Self::render(&out, polls, elapsed),
                    elapsed.as_secs_f64(),
                    if polls == 1 { "" } else { "s" },
                ))));
            }

            let remaining = Duration::from_secs(timeout.saturating_sub(elapsed.as_secs()));
            tokio::select! {
                () = tokio::time::sleep(remaining.min(Duration::from_secs(poll))) => {}
                () = cancel.cancelled() => {
                    return Ok(ToolOutcome::Immediate(ToolResult::error(
                        "wait_for: cancelled while waiting",
                    )));
                }
            }
        }
    }
}

/// Parse the `until` expression, defaulting to success-on-zero.
fn parse_until(raw: Option<&str>) -> Until {
    let Some(spec) = raw else {
        return Until::Success;
    };
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix("exit_code:")
        && let Ok(code) = rest.trim().parse::<i32>()
    {
        return Until::ExitCode(code);
    }
    if let Some(rest) = spec.strip_prefix("regex:") {
        return Until::Regex(rest.trim().to_owned());
    }
    if spec.eq_ignore_ascii_case("success") {
        return Until::Success;
    }
    Until::Success
}

/// Clamp an optional numeric argument into `[min, max]`, falling back to `default`.
fn clamp(raw: Option<u64>, default: u64, min: u64, max: u64) -> u64 {
    raw.unwrap_or(default).clamp(min, max)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Shell executor returning a scripted sequence of exit codes, one per
    /// call, repeating the last entry once exhausted.
    struct ScriptedExecutor {
        codes: Vec<i32>,
        stdout: Vec<String>,
        calls: Arc<AtomicU32>,
    }

    impl ScriptedExecutor {
        fn new(codes: Vec<i32>) -> Self {
            let n = codes.len();
            Self {
                codes,
                stdout: vec![String::new(); n],
                calls: Arc::new(AtomicU32::new(0)),
            }
        }

        fn with_stdout(mut self, out: &str) -> Self {
            for slot in &mut self.stdout {
                *slot = out.to_owned();
            }
            self
        }
    }

    #[async_trait]
    impl ShellExecutor for ScriptedExecutor {
        async fn execute(
            &self,
            _command: &str,
            _working_dir: &Path,
            _timeout: Option<Duration>,
            _cancel: CancellationToken,
            _input: Option<&str>,
        ) -> Result<ShellOutput> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
            let code = self.codes.get(i).copied().unwrap_or_else(|| {
                *self
                    .codes
                    .last()
                    .expect("scripted executor needs at least one code")
            });
            // Past the end of the script, keep reporting the last stdout so a
            // timeout test sees stable output on the final poll.
            let last = self.stdout.len().saturating_sub(1);
            Ok(ShellOutput {
                exit_code: code,
                stdout: self
                    .stdout
                    .get(i)
                    .or(self.stdout.get(last))
                    .cloned()
                    .unwrap_or_default(),
                stderr: String::new(),
            })
        }
    }

    fn tool(executor: ScriptedExecutor) -> WaitFor {
        WaitFor {
            root: SandboxRoot::new(std::path::PathBuf::from(".")).unwrap(),
            executor: Box::new(executor),
            denylist: CommandDenylist::default_powershell(),
        }
    }

    /// Run the tool and return the result.
    async fn run(tool: &WaitFor, args: serde_json::Value) -> Result<ToolResult> {
        let outcome = tool.execute(args, CancellationToken::new()).await?;
        match outcome {
            ToolOutcome::Immediate(r) => Ok(r),
            ToolOutcome::Streamed(_) => panic!("wait_for must not stream"),
        }
    }

    // ── predicates ───────────────────────────────────────────────────────

    #[test]
    fn success_matches_exit_zero() {
        let out = ShellOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(Until::Success.satisfied(&out));
        let out = ShellOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!Until::Success.satisfied(&out));
    }

    #[test]
    fn exit_code_predicate_matches_exact_code() {
        let until = Until::ExitCode(3);
        let out = ShellOutput {
            exit_code: 3,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(until.satisfied(&out));
        let out = ShellOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!until.satisfied(&out));
    }

    #[test]
    fn regex_predicate_is_case_insensitive_across_streams() {
        let until = Until::Regex("ready".to_owned());
        let out = ShellOutput {
            exit_code: 1,
            stdout: "Server READY".to_owned(),
            stderr: String::new(),
        };
        assert!(until.satisfied(&out));
        let out = ShellOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "ready soon".to_owned(),
        };
        assert!(until.satisfied(&out));
        let out = ShellOutput {
            exit_code: 0,
            stdout: "starting".to_owned(),
            stderr: String::new(),
        };
        assert!(!until.satisfied(&out));
    }

    #[test]
    fn parse_until_defaults_to_success() {
        assert_eq!(parse_until(None), Until::Success);
        assert_eq!(parse_until(Some("success")), Until::Success);
        assert_eq!(parse_until(Some("  Success ")), Until::Success);
        assert_eq!(parse_until(Some("garbage")), Until::Success);
    }

    #[test]
    fn parse_until_reads_exit_code_and_regex() {
        assert_eq!(parse_until(Some("exit_code: 3")), Until::ExitCode(3));
        assert_eq!(
            parse_until(Some("regex: READY")),
            Until::Regex("READY".to_owned())
        );
        // An unparseable code falls back rather than erroring.
        assert_eq!(parse_until(Some("exit_code: abc")), Until::Success);
    }

    #[test]
    fn clamp_keeps_out_of_range_values_inside_bounds() {
        // The default is clamped too — a default above `max` is honoured as
        // `max` rather than escaping the range.
        assert_eq!(clamp(None, 300, 1, 100), 100);
        assert_eq!(clamp(Some(0), 300, 1, 100), 1);
        assert_eq!(clamp(Some(9_999), 300, 1, 100), 100);
    }

    // ── execution ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn returns_immediately_when_already_satisfied() {
        let ex = ScriptedExecutor::new(vec![0]);
        let calls = Arc::clone(&ex.calls);
        let tool = tool(ex);
        let result = run(&tool, json!({ "command": "true" })).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must not poll again");
    }

    #[tokio::test]
    async fn polls_until_predicate_is_met() {
        let ex = ScriptedExecutor::new(vec![1, 1, 0]);
        let calls = Arc::clone(&ex.calls);
        let tool = tool(ex);
        let result = run(
            &tool,
            json!({ "command": "probe", "poll_secs": 1, "timeout_secs": 30 }),
        )
        .await
        .unwrap();
        assert!(!result.is_error);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exit_code_predicate_is_honoured_end_to_end() {
        let tool = tool(ScriptedExecutor::new(vec![1, 3]));
        let result = run(
            &tool,
            json!({ "command": "p", "until": "exit_code: 3", "poll_secs": 1, "timeout_secs": 30 }),
        )
        .await
        .unwrap();
        assert!(!result.is_error, "exit code 3 should satisfy exit_code: 3");
    }

    #[tokio::test]
    async fn regex_predicate_is_honoured_end_to_end() {
        let tool = tool(ScriptedExecutor::new(vec![1, 0]).with_stdout("all READY"));
        let result = run(
            &tool,
            json!({ "command": "p", "until": "regex: ready", "poll_secs": 1, "timeout_secs": 30 }),
        )
        .await
        .unwrap();
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn timeout_returns_is_error_with_last_output() {
        let tool = tool(ScriptedExecutor::new(vec![1]).with_stdout("still building"));
        let result = run(
            &tool,
            json!({ "command": "p", "poll_secs": 1, "timeout_secs": 1 }),
        )
        .await
        .unwrap();
        assert!(result.is_error, "timeout must be an error result");
        assert!(
            result.output.contains("timed out"),
            "message should explain the expiry: {}",
            result.output
        );
        assert!(
            result.output.contains("still building"),
            "last output must be preserved: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn timeout_does_not_hang() {
        let tool = tool(ScriptedExecutor::new(vec![1]));
        let start = Instant::now();
        run(
            &tool,
            json!({ "command": "p", "poll_secs": 1, "timeout_secs": 2 }),
        )
        .await
        .unwrap();
        let total = start.elapsed();
        assert!(
            total < Duration::from_secs(8),
            "wait should end near the timeout, took {total:?}"
        );
    }

    #[tokio::test]
    async fn denied_command_is_rejected_before_execution() {
        let ex = ScriptedExecutor::new(vec![0]);
        let calls = Arc::clone(&ex.calls);
        let mut tool = tool(ex);
        tool.denylist = CommandDenylist::default_powershell();
        let result = run(&tool, json!({ "command": "Remove-Item -Recurse C:\\" }))
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("denied"),
            "expected a denylist rejection, got: {}",
            result.output
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a denied command must never execute"
        );
    }

    #[tokio::test]
    async fn cancelling_interrupts_promptly() {
        // `poll_secs` is deliberately long relative to the cancel delay: the
        // wait cannot finish on its own before the assertion window closes, so
        // returning at all proves the token was consulted. A short poll_secs
        // would let this pass even with cancellation removed.
        let tool = tool(ScriptedExecutor::new(vec![1]));
        let token = CancellationToken::new();
        let cancel_for_task = token.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_for_task.cancel();
        });
        let began = Instant::now();
        let outcome = tool
            .execute(
                json!({ "command": "p", "poll_secs": 30, "timeout_secs": 600 }),
                token,
            )
            .await
            .unwrap();
        let waited = began.elapsed();
        handle.await.unwrap();
        let result = match outcome {
            ToolOutcome::Immediate(r) => r,
            ToolOutcome::Streamed(_) => panic!("no stream"),
        };
        assert!(
            waited < Duration::from_secs(5),
            "cancel must interrupt the wait, not run it out; took {waited:?} with a \
             30s poll interval and a 600s budget"
        );
        assert!(result.is_error);
        assert!(
            result.output.to_lowercase().contains("cancel"),
            "expected a cancellation message, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn reports_elapsed_and_poll_count() {
        let tool = tool(ScriptedExecutor::new(vec![0]));
        let result = run(&tool, json!({ "command": "p" })).await.unwrap();
        assert!(result.output.contains("1 poll"), "got: {}", result.output);
        assert!(result.output.contains("elapsed"), "got: {}", result.output);
    }

    #[tokio::test]
    async fn clamps_out_of_range_params() {
        // A zero timeout must not become a busy loop or an instant failure that
        // skips the first poll entirely.
        let ex = ScriptedExecutor::new(vec![0]);
        let calls = Arc::clone(&ex.calls);
        let tool = tool(ex);
        let result = run(
            &tool,
            json!({ "command": "p", "timeout_secs": 0, "poll_secs": 0 }),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "first poll must still run");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn missing_command_is_an_argument_error() {
        let tool = tool(ScriptedExecutor::new(vec![0]));
        let err = run(&tool, json!({})).await.unwrap_err();
        assert!(
            matches!(&err, rho_core::RhoError::Tool(msg) if msg.contains("command")),
            "expected a missing-argument error naming `command`, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn cancelled_before_start_returns_immediately() {
        let ex = ScriptedExecutor::new(vec![0]);
        let calls = Arc::clone(&ex.calls);
        let tool = tool(ex);
        let token = CancellationToken::new();
        token.cancel();
        let outcome = tool
            .execute(json!({ "command": "p" }), token)
            .await
            .unwrap();
        match outcome {
            ToolOutcome::Immediate(r) => assert!(r.is_error),
            ToolOutcome::Streamed(_) => panic!("no stream"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    // ── tool surface ────────────────────────────────────────────────────

    #[test]
    fn name_and_risk_match_run_command_class() {
        let tool = tool(ScriptedExecutor::new(vec![0]));
        assert_eq!(&*tool.name(), "wait_for");
        assert_eq!(tool.risk(), ToolRisk::Destructive);
    }

    #[test]
    fn schema_requires_command_and_documents_until() {
        let tool = tool(ScriptedExecutor::new(vec![0]));
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"], json!(["command"]));
        assert!(schema["properties"]["until"].is_object());
    }
}
