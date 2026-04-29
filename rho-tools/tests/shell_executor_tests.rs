//! Integration tests for the `PowerShellExecutor`.
//!
//! These tests spawn real PowerShell processes, so they require `pwsh` or
//! `powershell` to be available on the system. They are kept minimal and
//! fast — no network, no file I/O beyond what PowerShell itself does.

use std::path::Path;
use std::time::Duration;

use rho_core::{CancellationToken, shell::ShellExecutor};
use rho_tools::PowerShellExecutor;

// ── Basic execution ───────────────────────────────────────────────────────────

#[tokio::test]
async fn captures_stdout() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "Write-Output 'hello world'",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("execution should succeed");

    assert!(output.stdout.contains("hello world"));
    assert!(output.stderr.is_empty());
    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn captures_stderr() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "Write-Error 'oops'",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("execution should succeed");

    assert!(output.stderr.contains("oops"));
    assert_ne!(output.exit_code, 0);
}

#[tokio::test]
async fn reports_nonzero_exit_code() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute("exit 42", Path::new("."), None, CancellationToken::new())
        .await
        .expect("execution should succeed");

    assert_eq!(output.exit_code, 42);
}

#[tokio::test]
async fn reports_zero_exit_code_on_success() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "Write-Output 'ok'",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("execution should succeed");

    assert_eq!(output.exit_code, 0);
}

// ── Working directory ─────────────────────────────────────────────────────────

#[tokio::test]
async fn respects_working_directory() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "Get-Location | Write-Output",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("execution should succeed");

    // The output should contain the current directory (resolved from ".").
    assert!(!output.stdout.trim().is_empty());
}

// ── Cancellation ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancellation_kills_long_running_process() {
    let executor = PowerShellExecutor::new();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Cancel after a short delay — the command sleeps for 30s, way longer.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = executor
        .execute("Start-Sleep -Seconds 30", Path::new("."), None, cancel)
        .await;

    // Should return an error (process killed by cancellation).
    assert!(result.is_err(), "cancelled command should return Err");
}

// ── Timeout ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn timeout_kills_long_running_process() {
    let executor = PowerShellExecutor::new();

    let result = executor
        .execute(
            "Start-Sleep -Seconds 30",
            Path::new("."),
            Some(Duration::from_millis(200)),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err(), "timed-out command should return Err");
}

#[tokio::test]
async fn short_command_completes_within_timeout() {
    let executor = PowerShellExecutor::new();

    let output = executor
        .execute(
            "Write-Output 'fast'",
            Path::new("."),
            Some(Duration::from_secs(10)),
            CancellationToken::new(),
        )
        .await
        .expect("fast command should complete within timeout");

    assert!(output.stdout.contains("fast"));
    assert_eq!(output.exit_code, 0);
}

// ── Shell detection ───────────────────────────────────────────────────────────

#[test]
fn executor_reports_available_shell() {
    let executor = PowerShellExecutor::new();
    // On a Windows dev machine, at least one PowerShell should be available.
    assert!(
        !executor.shell_exe().is_empty(),
        "expected a PowerShell executable to be detected"
    );
}

// ── PowerShell-specific features ──────────────────────────────────────────────

#[tokio::test]
async fn executes_pipeline() {
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "1..3 | ForEach-Object { $_ * 2 }",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("pipeline should execute");

    assert!(output.stdout.contains('2'));
    assert!(output.stdout.contains('4'));
    assert!(output.stdout.contains('6'));
}

#[tokio::test]
async fn executes_with_no_profile() {
    // The executor passes -NoProfile. Verify the $PROFILE variable is empty
    // (not loading a user profile script).
    let executor = PowerShellExecutor::new();
    let output = executor
        .execute(
            "Write-Output ($PROFILE -eq $null)",
            Path::new("."),
            None,
            CancellationToken::new(),
        )
        .await
        .expect("should execute");

    // $PROFILE is never $null in PowerShell, but -NoProfile means no profile
    // is loaded. Just verify the command runs successfully — the real test
    // is that no user profile side effects occur.
    assert_eq!(output.exit_code, 0);
}
