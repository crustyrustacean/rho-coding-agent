//! Integration tests for the built-in tools.
//!
//! Each test creates a real temporary directory sandbox and exercises the tool
//! end-to-end. No network or model API is required.

use rho_core::{
    SandboxRoot, ShellOutput,
    tool::{CancellationToken, Tool, ToolOutcome},
};
use rho_tools::{ReadFile, RunCommand, WriteFile};
use std::fs;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (TempDir, SandboxRoot) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = SandboxRoot::new(dir.path()).expect("create sandbox root");
    (dir, root)
}

fn immediate_output(outcome: &ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Immediate(result) => result.output.clone(),
        ToolOutcome::Streamed(_) => panic!("unexpected streamed output"),
    }
}

fn immediate_is_error(outcome: &ToolOutcome) -> bool {
    match outcome {
        ToolOutcome::Immediate(result) => result.is_error,
        ToolOutcome::Streamed(_) => panic!("unexpected streamed output"),
    }
}

// ── ReadFile ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_file_returns_contents_wrapped_in_context() {
    let (dir, root) = setup();
    let path = dir.path().join("hello.txt");
    fs::write(&path, "hello world").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({ "path": path.to_str().unwrap() });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("<context>"),
        "output must be wrapped in <context>: {output}"
    );
    assert!(
        output.contains("hello world"),
        "output must contain file contents"
    );
    assert!(output.contains("</context>"));
}

#[tokio::test]
async fn read_file_rejects_path_outside_sandbox() {
    let (_dir, root) = setup();
    let outside = tempfile::tempdir().unwrap();
    let evil = outside.path().join("evil.txt");
    fs::write(&evil, "evil").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({ "path": evil.to_str().unwrap() });
    let result = tool.execute(args, CancellationToken::new()).await;

    assert!(result.is_err(), "reading outside sandbox must fail");
}

#[tokio::test]
async fn read_file_returns_error_for_missing_path_argument() {
    let (_dir, root) = setup();
    let tool = ReadFile { root };
    let result = tool
        .execute(serde_json::json!({}), CancellationToken::new())
        .await;
    assert!(result.is_err(), "missing argument must return Err");
}

#[tokio::test]
async fn read_file_is_risk_read() {
    let (_dir, root) = setup();
    assert_eq!(ReadFile { root }.risk(), rho_core::ToolRisk::Read);
}

// ── WriteFile ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn write_file_creates_file_in_sandbox() {
    let (dir, root) = setup();
    let path = dir.path().join("out.txt");

    let tool = WriteFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "content": "written by rho"
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    assert_eq!(fs::read_to_string(&path).unwrap(), "written by rho");
}

#[tokio::test]
async fn write_file_creates_parent_directories() {
    let (dir, root) = setup();
    let path = dir.path().join("a").join("b").join("c.txt");

    let tool = WriteFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "content": "nested"
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
}

#[tokio::test]
async fn write_file_rejects_path_outside_sandbox() {
    let (_dir, root) = setup();
    let outside = tempfile::tempdir().unwrap();
    let evil = outside.path().join("evil.txt");

    let tool = WriteFile { root };
    let args = serde_json::json!({
        "path": evil.to_str().unwrap(),
        "content": "evil"
    });
    let result = tool.execute(args, CancellationToken::new()).await;
    assert!(result.is_err(), "writing outside sandbox must fail");
}

#[tokio::test]
async fn write_file_is_risk_write() {
    let (_dir, root) = setup();
    assert_eq!(WriteFile { root }.risk(), rho_core::ToolRisk::Write);
}

#[tokio::test]
async fn write_file_overwrites_existing_file() {
    let (dir, root) = setup();
    let path = dir.path().join("existing.txt");
    fs::write(&path, "original").unwrap();

    let tool = WriteFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "content": "overwritten"
    });
    tool.execute(args, CancellationToken::new()).await.unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "overwritten");
}

// ── RunCommand with MockShellExecutor ─────────────────────────────────────────

use rho_test_helpers::MockShellExecutor;

#[tokio::test]
async fn run_command_delegates_to_executor() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("mock output", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock.clone()),
    };

    let args = serde_json::json!({ "command": "Get-Date" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(output.contains("mock output"));
    assert!(output.contains("exit_code: 0"));
    assert!(!immediate_is_error(&outcome));

    // Verify the executor received the command.
    let commands = mock.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0], "Get-Date");
}

#[tokio::test]
async fn run_command_formats_stderr_in_output() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("stdout", "stderr text", 1)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
    };

    let args = serde_json::json!({ "command": "bad-command" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(output.contains("stderr text"));
    assert!(output.contains("exit_code: 1"));
    assert!(immediate_is_error(&outcome));
}

#[tokio::test]
async fn run_command_returns_error_for_missing_command_argument() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
    };

    let result = tool
        .execute(serde_json::json!({}), CancellationToken::new())
        .await;
    assert!(result.is_err(), "missing command argument must return Err");
}

#[tokio::test]
async fn run_command_is_risk_destructive() {
    let (_dir, root) = setup();
    let mock = MockShellExecutor::new(vec![]);
    assert_eq!(
        RunCommand {
            root,
            executor: Box::new(mock)
        }
        .risk(),
        rho_core::ToolRisk::Destructive
    );
}

#[tokio::test]
async fn run_command_returns_cancelled_when_token_is_set() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
    };

    let cancel = CancellationToken::new();
    cancel.cancel();

    let args = serde_json::json!({ "command": "echo hi" });
    let outcome = tool.execute(args, cancel).await.unwrap();
    assert!(immediate_is_error(&outcome));
}

// ── Cancellation ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_file_respects_cancellation() {
    let (dir, root) = setup();
    let path = dir.path().join("file.txt");
    fs::write(&path, "content").unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let tool = ReadFile { root };
    let args = serde_json::json!({ "path": path.to_str().unwrap() });
    let outcome = tool.execute(args, cancel).await.unwrap();
    // Should return an error result (cancelled), not execute the I/O
    assert!(
        immediate_is_error(&outcome),
        "cancelled tool should return error result"
    );
}
