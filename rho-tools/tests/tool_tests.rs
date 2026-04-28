//! Integration tests for the built-in tools.
//!
//! Each test creates a real temporary directory sandbox and exercises the tool
//! end-to-end. No network or model API is required.

use rho_core::{
    SandboxRoot,
    tool::{CancellationToken, Tool, ToolOutcome},
};
use rho_tools::{ReadFile, WriteFile};
use std::fs;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup() -> (TempDir, SandboxRoot) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = SandboxRoot::new(dir.path()).expect("create sandbox root");
    (dir, root)
}

fn immediate_output(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Immediate(result) => result.output,
        ToolOutcome::Streamed(_) => panic!("unexpected streamed output"),
    }
}

fn immediate_is_error(outcome: ToolOutcome) -> bool {
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

    let output = immediate_output(outcome);
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

    assert!(!immediate_is_error(outcome));
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

    assert!(!immediate_is_error(outcome));
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
        immediate_is_error(outcome),
        "cancelled tool should return error result"
    );
}
