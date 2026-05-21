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

fn immediate_is_success(outcome: &ToolOutcome) -> bool {
    match outcome {
        ToolOutcome::Immediate(result) => !result.is_error,
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
    assert!(output.contains("<context:end>"));
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
        output.contains("fresh process"),
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
        output.contains("fresh process"),
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

// ── run_command cwd parameter ─────────────────────────────────────────────────

#[tokio::test]
async fn run_command_cwd_defaults_to_project_root() {
    let (dir, root) = setup();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock.clone()),
        denylist: CommandDenylist::default_powershell(),
    };

    // No cwd parameter — should use project root.
    let args = serde_json::json!({ "command": "Get-Date" });
    tool.execute(args, CancellationToken::new()).await.unwrap();

    let dirs = mock.working_dirs();
    assert_eq!(dirs.len(), 1);
    // Compare canonicalized paths to handle Windows UNC prefix differences.
    assert_eq!(
        dunce::canonicalize(&dirs[0]).unwrap(),
        dunce::canonicalize(dir.path()).unwrap()
    );
}

#[tokio::test]
async fn run_command_cwd_resolves_subdirectory() {
    let (dir, root) = setup();
    let subdir = dir.path().join("subproject");
    fs::create_dir(&subdir).unwrap();

    let mock = MockShellExecutor::new(vec![ShellOutput::new("ok", "", 0)]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock.clone()),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cargo check", "cwd": "subproject" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(!immediate_is_error(&outcome));
    let dirs = mock.working_dirs();
    assert_eq!(dirs.len(), 1);
    assert_eq!(
        dunce::canonicalize(&dirs[0]).unwrap(),
        dunce::canonicalize(&subdir).unwrap()
    );
}

#[tokio::test]
async fn run_command_cwd_rejects_path_outside_sandbox() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cargo check", "cwd": "../../etc" });
    let result = tool.execute(args, CancellationToken::new()).await;
    assert!(result.is_err(), "cwd outside sandbox must be rejected");
}

#[tokio::test]
async fn run_command_cwd_rejects_nonexistent_directory() {
    let (_dir, root) = setup();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cargo check", "cwd": "no-such-dir" });
    let result = tool.execute(args, CancellationToken::new()).await;
    // Should fail because the directory doesn't exist (sandbox validation rejects it).
    assert!(result.is_err(), "nonexistent cwd must be rejected");
}

#[tokio::test]
async fn run_command_cwd_rejects_file_as_directory() {
    let (dir, root) = setup();
    let file = dir.path().join("not-a-dir.txt");
    fs::write(&file, "content").unwrap();

    let mock = MockShellExecutor::new(vec![]);
    let tool = RunCommand {
        root,
        executor: Box::new(mock),
        denylist: CommandDenylist::default_powershell(),
    };

    let args = serde_json::json!({ "command": "cargo check", "cwd": "not-a-dir.txt" });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();
    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.contains("not a directory"),
        "expected 'not a directory' in: {output}"
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
    // Plain text should show the general exactness hint, not the regex hint.
    assert!(
        output.contains("Hint"),
        "plain-text miss should contain an exactness hint: {output}"
    );
    assert!(
        !output.contains("regex-like patterns"),
        "plain-text miss should not contain a regex hint: {output}"
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

// ── EditFile: regex-pattern hint diagnostics ─────────────────────────────────

#[tokio::test]
async fn edit_file_regex_old_text_shows_hint() {
    let (dir, root) = setup();
    let path = dir.path().join("code.rs");
    fs::write(&path, "fn main() {\n    println!(\"hello\");\n}").unwrap();

    let tool = EditFile { root };
    // Simulate the exact class of mistake from the failing session:
    // the model sends regex-like \s* and \n instead of literal text.
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": r#"fn main() {\s*println!("hello");\n}"#,
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
    assert!(
        output.contains("Hint"),
        "regex old_text should trigger a hint: {output}"
    );
    assert!(
        output.contains(r"\s*"),
        "hint should mention the offending pattern: {output}"
    );
    assert!(
        output.contains("not a regex"),
        "hint should tell the model it's not a regex: {output}"
    );

    // File should be unchanged.
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "fn main() {\n    println!(\"hello\");\n}"
    );
}

/// Reproducer for the exact failure shape from session `1778102100_a57ac869`:
/// the model sends `old_text` full of `\n` and `\s*` escape sequences.
#[tokio::test]
async fn edit_file_session_reproducer_regex_loop() {
    let (dir, root) = setup();
    let path = dir.path().join("main.rs");
    let real_content = "\
fn main() {
    greet_user();
}

fn greet_user() {
    println!(\"What is your name?\\n\");
    let mut name = String::new();
    std::io::stdin().read_line(&mut name).expect(\"Failed to read line\");
    let name = name.trim();
    println!(\"Hello, {}!\", name);
}";
    fs::write(&path, real_content).unwrap();

    let tool = EditFile { root };
    // This is a representative substring of the model's actual payload:
    // escaped \n and \s* instead of literal whitespace.
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": r#"fn greet_user() {\n\s*println!("What is your name?\\n");"#,
            "new_text": "fn greet_user() { /* replaced */ }"
        }]
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    assert!(immediate_is_error(&outcome));
    let output = immediate_output(&outcome);
    assert!(
        output.contains("not found"),
        "expected 'not found' in: {output}"
    );
    assert!(output.contains("Hint"), "should show regex hint: {output}");
    assert!(
        output.contains(r"\s*") || output.contains(r"\n"),
        "hint should identify the regex patterns: {output}"
    );

    // File must be untouched.
    assert_eq!(fs::read_to_string(&path).unwrap(), real_content);
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

// ── CargoCheck → EditFile → CargoCheck integration test ───────────────────────

/// Simulates the agent workflow: `CargoCheck` finds an error, `EditFile` fixes
/// it, `CargoCheck` confirms the fix. Uses mocked shell output for
/// `CargoCheck` and real file operations for `EditFile`.
#[tokio::test]
async fn cargo_check_edit_file_cargo_check_loop() {
    use rho_test_helpers::{FileTestEnv, MockShellExecutor};
    use rho_tools::{CargoCheck, EditFile};

    let env = FileTestEnv::new();
    let root_path = env.root().to_string_lossy().replace('\\', "/");

    // Create a Rust file with a type error.
    env.write_file("src/lib.rs", "fn greet() -> String {\n    42\n}\n");

    // Step 1: CargoCheck returns an error diagnostic.
    let check_error_ndjson = format!(
        r#"{{"reason":"compiler-message","package_id":"test","manifest_path":"test","target":{{"kind":["lib"],"crate_types":["lib"],"name":"mylib","src_path":"{root_path}/src/lib.rs","edition":"2021","doc":true,"doctest":true,"test":true}},"message":{{"message":"mismatched types","code":{{"code":"E0308"}},"level":"error","spans":[{{"file_name":"src/lib.rs","byte_start":23,"byte_end":25,"line_start":2,"line_end":2,"column_start":5,"column_end":7,"is_primary":true,"text":[],"label":"expected `String`, found integer","suggested_replacement":null,"suggestion_applicability":null,"expansion":null}}],"children":[],"rendered":"error[E0308]: mismatched types\n"}}}}"#
    );
    let mock1 = MockShellExecutor::new(vec![ShellOutput::new(
        check_error_ndjson,
        String::new(),
        101,
    )]);

    let check_tool = CargoCheck {
        root: env.sandbox().clone(),
        executor: Box::new(mock1),
    };

    let cancel = CancellationToken::new();
    let result1 = check_tool
        .execute(serde_json::json!({}), cancel.clone())
        .await
        .unwrap();

    match &result1 {
        ToolOutcome::Immediate(r) => {
            assert!(r.is_error, "first check should report error");
            assert!(r.output.contains("E0308"));
        }
        ToolOutcome::Streamed(_) => panic!("expected immediate"),
    }

    // Step 2: EditFile fixes the error.
    let edit_tool = EditFile {
        root: env.sandbox().clone(),
    };

    let lib_path = env.root().join("src").join("lib.rs");
    let edit_result = edit_tool
        .execute(
            serde_json::json!({
                "path": lib_path.to_str().unwrap(),
                "edits": [{
                    "old_text": "    42",
                    "new_text": "    String::from(\"hello\")"
                }]
            }),
            cancel.clone(),
        )
        .await
        .unwrap();

    match &edit_result {
        ToolOutcome::Immediate(r) => {
            assert!(!r.is_error, "edit should succeed: {}", r.output);
            assert!(r.output.contains("applied 1 edit(s)"));
        }
        ToolOutcome::Streamed(_) => panic!("expected immediate"),
    }

    // Verify the file was actually modified.
    let fixed_content = env.read_file("src/lib.rs");
    assert!(
        fixed_content.contains("String::from"),
        "file should contain the fix"
    );

    // Step 3: CargoCheck now returns clean.
    let mock2 = MockShellExecutor::new(vec![ShellOutput::new(
        r#"{"reason":"build-finished","success":true}"#.to_owned(),
        String::new(),
        0,
    )]);

    let check_tool2 = CargoCheck {
        root: env.sandbox().clone(),
        executor: Box::new(mock2),
    };

    let result2 = check_tool2
        .execute(serde_json::json!({}), cancel)
        .await
        .unwrap();

    match &result2 {
        ToolOutcome::Immediate(r) => {
            assert!(!r.is_error, "second check should be clean");
            assert!(r.output.contains("no errors or warnings"));
        }
        ToolOutcome::Streamed(_) => panic!("expected immediate"),
    }
}

// ── Hashline ReadFile Tests (Phase 3.10) ────────────────────────────────────────

#[tokio::test]
async fn read_file_with_hashline_enabled_outputs_hashline_format() {
    let (dir, root) = setup();
    let path = dir.path().join("hello.txt");
    fs::write(&path, "hello world").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("<context>"),
        "output must be wrapped in <context>: {output}"
    );
    assert!(
        output.contains("1#"),
        "output must contain line number with hash: {output}"
    );
    assert!(output.contains("<context:end>"));
}

#[tokio::test]
async fn read_file_with_hashline_disabled_outputs_legacy_format() {
    let (dir, root) = setup();
    let path = dir.path().join("hello.txt");
    fs::write(&path, "hello world").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": false
    });
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
    assert!(
        !output.contains("1#"),
        "hashline format must not be present: {output}"
    );
    assert!(output.contains("<context:end>"));
}

#[tokio::test]
async fn read_file_without_hashline_parameter_defaults_to_hashline_format() {
    let (dir, root) = setup();
    let path = dir.path().join("hello.txt");
    fs::write(&path, "hello world").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap()
        // No hashline parameter - should default to true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("1#"),
        "default behavior should be hashline format: {output}"
    );
}

#[tokio::test]
async fn read_file_hashline_format_correct_line_number_padding() {
    let (dir, root) = setup();
    let path = dir.path().join("many.txt");
    let content = (1..=100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, content).unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    // Line 1 should be "  1#" (width=3 for 100 lines)
    assert!(
        output.contains("  1#"),
        "line 1 should be padded to width 3: {output}"
    );
    // Line 100 should be "100#" (width=3)
    assert!(
        output.contains("100#"),
        "line 100 should be padded to width 3: {output}"
    );
}

#[tokio::test]
async fn read_file_hashline_format_empty_file() {
    let (dir, root) = setup();
    let path = dir.path().join("empty.txt");
    fs::write(&path, "").unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    // Empty file should show just framing, no line numbers
    assert!(
        output.contains("<context>"),
        "output must be wrapped in <context>: {output}"
    );
    assert!(
        output.contains("<context:end>"),
        "output must contain <context:end>: {output}"
    );
    // Should not contain any line numbers
    assert!(
        !output.contains('#'),
        "empty file should not contain line numbers: {output}"
    );
}

#[tokio::test]
async fn read_file_hashline_multiline_content() {
    let (dir, root) = setup();
    let path = dir.path().join("multi.txt");
    let content = "function hello() {\n  console.log(\"world\");\n}\n";
    fs::write(&path, content).unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(output.contains("1#"), "should have line 1: {output}");
    assert!(output.contains("2#"), "should have line 2: {output}");
    assert!(output.contains("3#"), "should have line 3: {output}");
    assert!(
        output.contains("function hello() {"),
        "should contain line 1 content: {output}"
    );
    assert!(
        output.contains("console.log(\"world\");"),
        "should contain line 2 content: {output}"
    );
    assert!(
        output.contains('}'),
        "should contain line 3 content: {output}"
    );
}

#[tokio::test]
async fn read_file_hashline_format_preserves_whitespace() {
    let (dir, root) = setup();
    let path = dir.path().join("whitespace.txt");
    let content = "  indented\n\ttabbed\n  mixed\n";
    fs::write(&path, content).unwrap();

    let tool = ReadFile { root };
    let args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let outcome = tool.execute(args, CancellationToken::new()).await.unwrap();

    let output = immediate_output(&outcome);
    assert!(
        output.contains("  indented"),
        "should preserve indentation: {output}"
    );
    assert!(
        output.contains("\ttabbed"),
        "should preserve tabs: {output}"
    );
    assert!(
        output.contains("  mixed"),
        "should preserve mixed whitespace: {output}"
    );
}

// ── Hashline EditFile Tests (Phase 3.10) ───────────────────────────────────────

#[tokio::test]
async fn edit_file_hashline_replace_single_line_with_valid_hash() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3";
    fs::write(&path, content).unwrap();

    // First, read the file to get the hash
    let read_tool = ReadFile { root: root.clone() };
    let read_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "hashline": true
    });
    let read_outcome = read_tool
        .execute(read_args, CancellationToken::new())
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);

    // Extract the hash for line 2 (e.g., "1#...:line 1\n2#...:line 2")
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| {
            line.split('#')
                .nth(1)
                .and_then(|hash_part| hash_part.split(':').next())
        });

    let hash = line_2_hash.expect("should find hash for line 2");

    let edit_tool = EditFile { root };
    let anchor = format!("2#{hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": anchor,
            "lines": ["modified line 2"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(!immediate_is_error(&edit_outcome), "edit should succeed");
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\nmodified line 2\nline 3");
}

#[tokio::test]
async fn edit_file_hashline_mismatch_with_high_information_line_applies_with_relaxation() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "original line 2";
    fs::write(&path, content).unwrap();

    let edit_tool = EditFile { root };
    // Use an incorrect hash — but the line is high-information,
    // so fuzzy matching applies it with relaxation (Tier 2).
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "1#ZZ",  // Wrong hash
            "lines": ["modified"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(
        immediate_is_success(&edit_outcome),
        "edit should succeed with anchor relaxation"
    );
    assert!(
        output.contains("anchor relaxation"),
        "output should mention anchor relaxation"
    );
    assert!(
        output.contains("hash stale"),
        "output should warn about stale hash"
    );
    // Verify the edit was actually applied
    let actual = fs::read_to_string(&path).unwrap();
    assert_eq!(actual, "modified");
}

#[tokio::test]
async fn edit_file_hashline_mismatch_with_low_information_line_hard_fails() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    // Low-information line: just "}" — not enough signal for fuzzy matching
    let content = "}";
    fs::write(&path, content).unwrap();

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "1#ZZ",  // Wrong hash, and line is too short for fuzzy match
            "lines": ["modified"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(
        immediate_is_error(&edit_outcome),
        "edit should hard-fail for low-information line with wrong hash"
    );
    assert!(
        output.contains("hash mismatch"),
        "error should mention hash mismatch"
    );
    assert!(
        output.contains("Fresh hashes"),
        "error should include fresh hashes context"
    );
}

#[tokio::test]
async fn edit_file_hashline_append_after_anchor() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3";
    fs::write(&path, content).unwrap();

    // Get hash for line 2
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash");

    let edit_tool = EditFile { root };
    let anchor = format!("2#{line_2_hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "append",
            "pos": anchor,
            "lines": ["inserted after line 2"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(!immediate_is_error(&edit_outcome), "edit should succeed");
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\nline 2\ninserted after line 2\nline 3");
}

#[tokio::test]
async fn edit_file_hashline_prepend_before_anchor() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3";
    fs::write(&path, content).unwrap();

    // Get hash for line 2
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash");

    let edit_tool = EditFile { root };
    let anchor = format!("2#{line_2_hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "prepend",
            "pos": anchor,
            "lines": ["inserted before line 2"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(!immediate_is_error(&edit_outcome), "edit should succeed");
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\ninserted before line 2\nline 2\nline 3");
}

#[tokio::test]
async fn edit_file_hashline_delete_line_at_anchor() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4";
    fs::write(&path, content).unwrap();

    // Get hash for line 2
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash");

    let edit_tool = EditFile { root };
    let anchor = format!("2#{line_2_hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "delete",
            "pos": anchor
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(!immediate_is_error(&edit_outcome), "edit should succeed");
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\nline 3\nline 4");
}

#[tokio::test]
async fn edit_file_legacy_format_still_works() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "hello world";
    fs::write(&path, content).unwrap();

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "old_text": "hello world",
            "new_text": "hello hashline"
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !immediate_is_error(&edit_outcome),
        "legacy edit should succeed"
    );
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "hello hashline");
}

#[tokio::test]
async fn edit_file_mixed_hashline_and_legacy_edits() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4";
    fs::write(&path, content).unwrap();

    // Get hash for line 1
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_1_hash = read_output
        .lines()
        .find(|line| line.contains("1#") && line.contains("line 1"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash");

    let edit_tool = EditFile { root };
    let anchor = format!("1#{line_1_hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [
            {
                "op": "replace",
                "pos": anchor,
                "lines": ["modified line 1"]
            },
            {
                "old_text": "line 3",
                "new_text": "line three"
            }
        ]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !immediate_is_error(&edit_outcome),
        "mixed edits should succeed"
    );
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "modified line 1\nline 2\nline three\nline 4");
}

#[tokio::test]
async fn edit_file_hashline_success_includes_diff() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4";
    fs::write(&path, content).unwrap();

    // Get hash for line 2
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash");

    let edit_tool = EditFile { root };
    let anchor = format!("2#{line_2_hash}");
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": anchor,
            "lines": ["modified line 2"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(!immediate_is_error(&edit_outcome), "edit should succeed");
    assert!(
        output.contains("<diff>"),
        "output should include diff: {output}"
    );
    assert!(output.contains("</diff>"), "output should close diff tag");
    assert!(
        output.contains('+'),
        "diff should contain added line marker"
    );
}

#[tokio::test]
async fn edit_file_hashline_invalid_op_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    fs::write(&path, "content").unwrap();

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "invalid",
            "pos": "1#XX",
            "lines": ["x"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(immediate_is_error(&edit_outcome), "should fail");
    assert!(
        output.contains("invalid op"),
        "should mention invalid op: {output}"
    );
}

#[tokio::test]
async fn edit_file_hashline_out_of_range_returns_error() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    fs::write(&path, "line 1\nline 2").unwrap();

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "99#XX",
            "lines": ["x"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(immediate_is_error(&edit_outcome), "should fail");
    assert!(
        output.contains("out of range"),
        "should mention out of range: {output}"
    );
}

#[tokio::test]
async fn edit_file_hashline_invalid_anchor_format() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    fs::write(&path, "content").unwrap();

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "invalid",
            "lines": ["x"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(immediate_is_error(&edit_outcome), "should fail");
    assert!(
        output.contains("invalid anchor"),
        "should mention invalid anchor: {output}"
    );
}

#[tokio::test]
async fn edit_file_hashline_delete_range() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4\nline 5";
    fs::write(&path, content).unwrap();

    // Get hashes for lines 2 and 4
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);

    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 2");
    let line_4_hash = read_output
        .lines()
        .find(|line| line.contains("4#") && line.contains("line 4"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 4");

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "delete",
            "pos": format!("2#{line_2_hash}"),
            "end": format!("4#{line_4_hash}")
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !immediate_is_error(&edit_outcome),
        "delete range should succeed"
    );
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\nline 5");
}

#[tokio::test]
async fn edit_file_hashline_replace_range() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4\nline 5";
    fs::write(&path, content).unwrap();

    // Get hashes
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);

    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("hash 2");
    let line_4_hash = read_output
        .lines()
        .find(|line| line.contains("4#") && line.contains("line 4"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("hash 4");

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("2#{line_2_hash}"),
            "end": format!("4#{line_4_hash}"),
            "lines": ["replaced a", "replaced b"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        !immediate_is_error(&edit_outcome),
        "replace range should succeed"
    );
    let modified = fs::read_to_string(&path).unwrap();
    assert_eq!(modified, "line 1\nreplaced a\nreplaced b\nline 5");
}

// ── Phase A: Fresh Anchors Block ─────────────────────────────────────────────

#[tokio::test]
async fn edit_file_hashline_success_includes_fresh_anchors_block() {
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\nline 2\nline 3\nline 4\nline 5";
    fs::write(&path, content).unwrap();

    // Get hash for line 3
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_3_hash = read_output
        .lines()
        .find(|line| line.contains("3#") && line.contains("line 3"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 3");

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("3#{line_3_hash}"),
            "lines": ["modified line 3"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(
        immediate_is_success(&edit_outcome),
        "edit should succeed"
    );
    assert!(
        output.contains("<fresh-anchors>"),
        "output should include fresh-anchors block: {output}"
    );
    assert!(
        output.contains("</fresh-anchors>"),
        "output should close fresh-anchors tag"
    );
    assert!(
        output.contains("fresh anchors"),
        "output should mention fresh anchors"
    );
}

#[tokio::test]
async fn edit_file_hashline_chained_edit_with_stale_hash_succeeds() {
    // Simulates the retry-spiral scenario: make edit A, then use stale
    // anchors from the original read to make edit B. With fuzzy matching,
    // edit B should succeed with relaxation (no retry needed).
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "line 1\noriginal line 2\nline 3\nline 4";
    fs::write(&path, content).unwrap();

    // Step 1: Read file to get original hashes
    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);

    // Extract original hash for line 2
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("original line 2"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 2");
    let _line_4_hash = read_output
        .lines()
        .find(|line| line.contains("4#") && line.contains("line 4"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 4");

    // Step 2: Make edit A — replace line 2 (this invalidates line 2's hash)
    let edit_tool = EditFile { root: root.clone() };
    let edit_a_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("2#{line_2_hash}"),
            "lines": ["modified line 2"]
        }]
    });
    let edit_a_outcome = edit_tool
        .execute(edit_a_args, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        immediate_is_success(&edit_a_outcome),
        "edit A should succeed"
    );

    // Step 3: Make edit B using the ORIGINAL (now stale) hash for line 4.
    // Line 4 hasn't actually changed (line 2 was modified, not line 4),
    // so the hash is still valid in this case. But let's test the case
    // where we use a completely wrong hash for a high-information line.
    let edit_b_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("4#ZZ"),  // Stale/wrong hash
            "lines": ["modified line 4"]
        }]
    });
    let second_edit_outcome = edit_tool
        .execute(edit_b_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&second_edit_outcome);
    assert!(
        immediate_is_success(&second_edit_outcome),
        "edit B should succeed with anchor relaxation (stale hash on high-info line)"
    );
    assert!(
        output.contains("anchor relaxation"),
        "output should mention anchor relaxation: {output}"
    );
    // Verify the edit was applied correctly
    let final_content = fs::read_to_string(&path).unwrap();
    assert_eq!(
        final_content,
        "line 1\nmodified line 2\nline 3\nmodified line 4"
    );
}

#[tokio::test]
async fn edit_file_hashline_diff_shows_old_content_on_minus_lines() {
    // Fix 3: The diff's '-' lines should show OLD content, not new content
    let (dir, root) = setup();
    let path = dir.path().join("test.txt");
    let content = "alpha\nbeta\ngamma";
    fs::write(&path, content).unwrap();

    let read_tool = ReadFile { root: root.clone() };
    let read_outcome = read_tool
        .execute(
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "hashline": true
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let read_output = immediate_output(&read_outcome);
    let line_2_hash = read_output
        .lines()
        .find(|line| line.contains("2#") && line.contains("beta"))
        .and_then(|line| line.split('#').nth(1).and_then(|h| h.split(':').next()))
        .expect("should find hash for line 2");

    let edit_tool = EditFile { root };
    let edit_args = serde_json::json!({
        "path": path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("2#{line_2_hash}"),
            "lines": ["BETA"]
        }]
    });
    let edit_outcome = edit_tool
        .execute(edit_args, CancellationToken::new())
        .await
        .unwrap();

    let output = immediate_output(&edit_outcome);
    assert!(immediate_is_success(&edit_outcome));

    // Extract diff lines
    let diff_start = output.find("<diff>").expect("should have diff");
    let diff_end = output.find("</diff>").expect("should close diff");
    let diff = &output[diff_start..diff_end];

    // The '-' line should contain "beta" (old content)
    // The '+' line should contain "BETA" (new content)
    let minus_line = diff.lines().find(|l| l.starts_with('-')).expect("should have - line");
    let plus_line = diff.lines().find(|l| l.starts_with('+')).expect("should have + line");

    assert!(
        minus_line.contains("beta"),
        "'- 'line should show OLD content (beta): {minus_line}"
    );
    assert!(
        !minus_line.contains("BETA"),
        "'- 'line should NOT show NEW content (BETA): {minus_line}"
    );
    assert!(
        plus_line.contains("BETA"),
        "'+ 'line should show NEW content (BETA): {plus_line}"
    );
}
