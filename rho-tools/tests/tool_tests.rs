//! Integration tests for the built-in tools.
//!
//! Each test creates a real temporary directory sandbox and exercises the tool
//! end-to-end. No network or model API is required.

use rho_core::{
    SandboxRoot, ShellOutput,
    tool::{CancellationToken, Tool, ToolOutcome},
};
use rho_tools::{CommandDenylist, EditFile, ListDir, ReadFile, RunCommand, WriteFile};
use std::fs;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a sandboxed temp directory for tools that need a `SandboxRoot`.
///
/// For tools that also need to create/read files, use
/// [`rho_test_helpers::FileTestEnv`] instead.
fn setup() -> (TempDir, SandboxRoot) {
    rho_test_helpers::tempdir_with_sandbox()
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
        denylist: CommandDenylist::default_powershell(),
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
        denylist: CommandDenylist::default_powershell(),
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
        denylist: CommandDenylist::default_powershell(),
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
            executor: Box::new(mock),
            denylist: CommandDenylist::default_powershell(),
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
        denylist: CommandDenylist::default_powershell(),
    };

    let cancel = CancellationToken::new();
    cancel.cancel();

    let args = serde_json::json!({ "command": "echo hi" });
    let outcome = tool.execute(args, cancel).await.unwrap();
    assert!(immediate_is_error(&outcome));
}

// ── CommandDenylist ──────────────────────────────────────────────────────────

#[test]
fn denylist_blocks_remove_item() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Remove-Item foo.txt").is_some());
}

#[test]
fn denylist_blocks_invoke_webrequest() {
    let denylist = CommandDenylist::default_powershell();
    assert!(
        denylist
            .check("Invoke-WebRequest https://evil.com")
            .is_some()
    );
}

#[test]
fn denylist_blocks_invoke_restmethod() {
    let denylist = CommandDenylist::default_powershell();
    assert!(
        denylist
            .check("Invoke-RestMethod https://evil.com")
            .is_some()
    );
}

#[test]
fn denylist_blocks_start_process() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Start-Process notepad").is_some());
}

#[test]
fn denylist_blocks_new_service() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("New-Service -Name evil").is_some());
}

#[test]
fn denylist_blocks_set_executionpolicy() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Set-ExecutionPolicy Unrestricted").is_some());
}

#[test]
fn denylist_blocks_recurse_force_combo() {
    let denylist = CommandDenylist::default_powershell();
    // The -Recurse -Force combination is denied regardless of command.
    assert!(denylist.check("Get-ChildItem -Recurse -Force").is_some());
}

#[test]
fn denylist_blocks_force_recurse_combo() {
    let denylist = CommandDenylist::default_powershell();
    // Order doesn't matter: -Force -Recurse is also denied.
    assert!(denylist.check("Get-ChildItem -Force -Recurse").is_some());
}

#[test]
fn denylist_allows_safe_commands() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Get-ChildItem").is_none());
    assert!(denylist.check("Write-Output 'hello'").is_none());
    assert!(denylist.check("Get-Content file.txt").is_none());
}

#[test]
fn denylist_is_case_insensitive() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("remove-item foo").is_some());
    assert!(denylist.check("REMOVE-ITEM foo").is_some());
    assert!(denylist.check("Remove-Item foo").is_some());
}

#[test]
fn denylist_reason_mentions_denied_command() {
    let denylist = CommandDenylist::default_powershell();
    let reason = denylist.check("Remove-Item foo").unwrap();
    assert!(
        reason.to_lowercase().contains("remove-item"),
        "reason should mention the denied command: {reason}"
    );
}

#[test]
fn denylist_recurse_without_force_is_allowed() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Get-ChildItem -Recurse").is_none());
}

#[test]
fn denylist_force_without_recurse_is_allowed() {
    let denylist = CommandDenylist::default_powershell();
    assert!(denylist.check("Stop-Process -Force").is_none());
}

// ── RunCommand denylist enforcement ────────────────────────────────────────────

#[tokio::test]
async fn run_command_denies_blacklisted_command() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock.clone()),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "Remove-Item foo.txt" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.to_lowercase().contains("denied"),
        "output should mention denial: {output}"
    );

    // The executor should never have been called.
    assert!(mock.commands().is_empty());
}

#[tokio::test]
async fn run_command_allows_safe_command() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock.clone()),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "Get-ChildItem" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    assert_eq!(mock.commands().len(), 1);
}

// ── Working directory escape detection ─────────────────────────────────────────

#[tokio::test]
async fn run_command_warns_on_cd_with_dotdot() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cd .." });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    // The command should still execute (warning, not denial),
    // but the output should include a warning prefix.
    let output = immediate_output(&outcome);
    assert!(
        output.contains("[WARNING"),
        "output should contain working directory warning: {output}"
    );
}

#[tokio::test]
async fn run_command_warns_on_set_location_with_dotdot() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "Set-Location .." });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("[WARNING"),
        "output should contain working directory warning: {output}"
    );
}

#[tokio::test]
async fn run_command_no_warning_for_cd_within_sandbox() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cd src" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        !output.contains("[WARNING"),
        "output should not contain working directory warning: {output}"
    );
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

// ── ListDir ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_dir_lists_files_in_root() {
    let (dir, root) = setup();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.rs"), "b").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({});
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(!immediate_is_error(&outcome));
    assert!(output.contains("a.txt"), "expected a.txt in: {output}");
    assert!(output.contains("b.rs"), "expected b.rs in: {output}");
}

#[tokio::test]
async fn list_dir_marks_directories_with_trailing_slash() {
    let (dir, root) = setup();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src").join("main.rs"), "fn main()").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({});
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("src/"),
        "directory should have trailing slash: {output}"
    );
    // Non-recursive: should NOT list files inside src/
    assert!(
        !output.contains("main.rs"),
        "non-recursive should not list nested files: {output}"
    );
}

#[tokio::test]
async fn list_dir_recursive_lists_nested_files() {
    let (dir, root) = setup();
    fs::create_dir_all(dir.path().join("src").join("utils")).unwrap();
    fs::write(dir.path().join("src").join("main.rs"), "fn main()").unwrap();
    fs::write(
        dir.path().join("src").join("utils").join("helpers.rs"),
        "pub fn help()",
    )
    .unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({ "recursive": true });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(output.contains("src/"), "expected src/ in: {output}");
    assert!(
        output.contains("src\\main.rs") || output.contains("src/main.rs"),
        "expected main.rs in: {output}"
    );
    assert!(output.contains("utils"), "expected utils in: {output}");
    assert!(
        output.contains("helpers.rs"),
        "expected helpers.rs in: {output}"
    );
}

#[tokio::test]
async fn list_dir_respects_gitignore() {
    let (dir, root) = setup();
    fs::write(dir.path().join("tracked.txt"), "visible").unwrap();
    fs::write(dir.path().join("ignored.log"), "hidden").unwrap();
    fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({});
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("tracked.txt"),
        "expected tracked.txt in: {output}"
    );
    assert!(
        !output.contains("ignored.log"),
        "ignored file should not appear: {output}"
    );
    // .gitignore itself should appear
    assert!(
        output.contains(".gitignore"),
        "expected .gitignore in: {output}"
    );
}

#[tokio::test]
async fn list_dir_shows_hidden_files() {
    let (dir, root) = setup();
    fs::write(dir.path().join(".hidden"), "secret").unwrap();
    fs::write(dir.path().join("visible.txt"), "hello").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({});
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains(".hidden"),
        "hidden files should appear: {output}"
    );
    assert!(
        output.contains("visible.txt"),
        "expected visible.txt in: {output}"
    );
}

#[tokio::test]
async fn list_dir_subdirectory() {
    let (dir, root) = setup();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("file.txt"), "hi").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({ "path": sub.to_str().unwrap() });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("file.txt"),
        "expected file.txt in: {output}"
    );
}

#[tokio::test]
async fn list_dir_empty_directory() {
    let (dir, root) = setup();
    let sub = dir.path().join("empty");
    fs::create_dir(&sub).unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({ "path": sub.to_str().unwrap() });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("empty directory"),
        "expected empty directory message: {output}"
    );
}

#[tokio::test]
async fn list_dir_rejects_path_outside_sandbox() {
    let (_dir, root) = setup();
    let outside = tempfile::tempdir().unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({ "path": outside.path().to_str().unwrap() });
    let result = tool.execute(args, CancellationToken::new()).await;
    assert!(result.is_err(), "listing outside sandbox must fail");
}

#[tokio::test]
async fn list_dir_not_a_directory_returns_error() {
    let (dir, root) = setup();
    let file = dir.path().join("file.txt");
    fs::write(&file, "hello").unwrap();

    let tool = ListDir { root };
    let args = serde_json::json!({ "path": file.to_str().unwrap() });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
}

#[tokio::test]
async fn list_dir_is_risk_read() {
    let (_dir, root) = setup();
    assert_eq!(ListDir { root }.risk(), rho_core::ToolRisk::Read);
}

#[tokio::test]
async fn list_dir_respects_cancellation() {
    let (dir, root) = setup();
    fs::write(dir.path().join("file.txt"), "content").unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let tool = ListDir { root };
    let args = serde_json::json!({});
    let outcome = tool.execute(args, cancel).await.unwrap();
    assert!(immediate_is_error(&outcome));
}

// ── EditFile ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn edit_file_single_replacement() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "fn hello() { println!(\"hi\"); }").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": "println!(\"hi\")",
            "new_text": "println!(\"hello\")"
        }]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(output.contains("1 edit"));

    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "fn hello() { println!(\"hello\"); }");
}

#[tokio::test]
async fn edit_file_multiple_non_overlapping_edits() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "let a = 1;\nlet b = 2;\nlet c = 3;").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [
            { "old_text": "let a = 1", "new_text": "let a = 10" },
            { "old_text": "let c = 3", "new_text": "let c = 30" }
        ]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "let a = 10;\nlet b = 2;\nlet c = 30;");
}

#[tokio::test]
async fn edit_file_old_text_not_found_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "fn main() {}").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": "nonexistent text",
            "new_text": "replacement"
        }]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.contains("not found"),
        "expected 'not found' in: {output}"
    );

    // File should be unchanged.
    assert_eq!(fs::read_to_string(&path).unwrap(), "fn main() {}");
}

#[tokio::test]
async fn edit_file_ambiguous_match_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "let x = 1; let x = 2;").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": "let x",
            "new_text": "let y"
        }]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.contains("ambiguous"),
        "expected 'ambiguous' in: {output}"
    );

    // File should be unchanged.
    assert_eq!(fs::read_to_string(&path).unwrap(), "let x = 1; let x = 2;");
}

#[tokio::test]
async fn edit_file_overlapping_edits_return_error() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "abcdef").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [
            { "old_text": "abcd", "new_text": "ABCD" },
            { "old_text": "cdef", "new_text": "CDEF" }
        ]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.contains("overlap"),
        "expected 'overlap' in: {output}"
    );

    // File should be unchanged.
    assert_eq!(fs::read_to_string(&path).unwrap(), "abcdef");
}

#[tokio::test]
async fn edit_file_empty_edits_array_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "hello").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": []
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
}

#[tokio::test]
async fn edit_file_missing_path_returns_error() {
    let (_dir, root) = setup();
    let tool = EditFile { root };
    let args = serde_json::json!({
        "edits": [{ "old_text": "a", "new_text": "b" }]
    });
    let result = tool.execute(args, CancellationToken::new()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn edit_file_rejects_path_outside_sandbox() {
    let (_dir, root) = setup();
    let outside = tempfile::tempdir().unwrap();
    let evil = outside.path().join("evil.rs");
    fs::write(&evil, "evil").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": evil.to_str().unwrap(),
        "edits": [{ "old_text": "evil", "new_text": "good" }]
    });
    let result = tool.execute(args, CancellationToken::new()).await;
    assert!(result.is_err(), "editing outside sandbox must fail");
}

#[tokio::test]
async fn edit_file_is_risk_write() {
    let (_dir, root) = setup();
    assert_eq!(EditFile { root }.risk(), rho_core::ToolRisk::Write);
}

#[tokio::test]
async fn edit_file_respects_cancellation() {
    let (dir, root) = setup();
    let path = dir.path().join("file.rs");
    fs::write(&path, "content").unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{ "old_text": "content", "new_text": "new" }]
    });
    let outcome = tool.execute(args, cancel).await.unwrap();
    assert!(immediate_is_error(&outcome));
}

#[tokio::test]
async fn edit_file_file_not_found_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("nonexistent.rs");

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{ "old_text": "x", "new_text": "y" }]
    });
    let result = tool.execute(args, CancellationToken::new()).await;
    // EditFile uses validate() (not validate_for_write), which requires the
    // file to exist. A non-existent path fails at sandbox validation,
    // returning Err — this is correct: EditFile can only edit existing files.
    assert!(result.is_err());
}

#[tokio::test]
async fn edit_file_deletion_with_empty_new_text() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "line1\nline2\nline3").unwrap();

    let tool = EditFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": "line2\n",
            "new_text": ""
        }]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    assert_eq!(fs::read_to_string(&path).unwrap(), "line1\nline3");
}

// ── CommandDenylist::from_config ───────────────────────────────────────────────

#[test]
fn denylist_from_config_includes_builtins_and_extras() {
    use rho_core::{RhoConfig, ShellConfig};

    let config = RhoConfig {
        shell: ShellConfig {
            denied_commands: vec!["Stop-Process".to_owned()],
            denied_flag_combos: vec![vec!["-Quiet".to_owned(), "-Force".to_owned()]],
        },
        ..Default::default()
    };

    let denylist = CommandDenylist::from_config(&config);

    // Built-in denylist entries are still present.
    assert!(denylist.check("Remove-Item foo").is_some());
    assert!(denylist.check("Invoke-WebRequest https://x").is_some());

    // Config-supplied entries are added.
    assert!(denylist.check("Stop-Process notepad").is_some());

    // Config-supplied flag combo is added.
    assert!(denylist.check("Get-ChildItem -Quiet -Force").is_some());

    // Safe commands are still allowed.
    assert!(denylist.check("Get-ChildItem").is_none());
    assert!(denylist.check("Write-Output 'hello'").is_none());
}

#[test]
fn denylist_from_config_case_insensitive() {
    use rho_core::{RhoConfig, ShellConfig};

    let config = RhoConfig {
        shell: ShellConfig {
            denied_commands: vec!["STOP-PROCESS".to_owned()],
            denied_flag_combos: vec![],
        },
        ..Default::default()
    };

    let denylist = CommandDenylist::from_config(&config);

    // Config-supplied commands are case-insensitive.
    assert!(denylist.check("stop-process notepad").is_some());
    assert!(denylist.check("Stop-Process notepad").is_some());
}
