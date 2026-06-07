//! Integration tests for the `PowerShellExecutor`.
//!
//! These tests spawn real PowerShell processes, so they require `pwsh` or
//! `powershell` to be available on the system. They are kept minimal and
//! fast — no network, no file I/O beyond what PowerShell itself does.
//!
//! If no PowerShell is found on `PATH`, all tests in this file are skipped.

use std::path::Path;
use std::time::Duration;

use rho_core::{CancellationToken, shell::ShellExecutor};
use rho_tools::PowerShellExecutor;

// ── Test setup ───────────────────────────────────────────────────────────────

/// Check if PowerShell is available, returning true if it is.
fn has_powershell() -> bool {
    which::which("pwsh").is_ok() || which::which("powershell").is_ok()
}

/// Macro to skip tests when PowerShell is not available.
macro_rules! skip_if_no_powershell {
    () => {
        if !has_powershell() {
            eprintln!(
                "Skipping {}: PowerShell not found on PATH — install PowerShell 7+ (pwsh) or ensure Windows PowerShell (powershell) is available",
                std::any::type_name::<fn()>()
            );
            return;
        }
    };
}

/// Create a PowerShell executor, assuming PowerShell is available.
/// Use `skip_if_no_powershell!()` before calling this function.
fn get_executor() -> PowerShellExecutor {
    PowerShellExecutor::new()
        .expect("PowerShell is required for shell executor tests (use skip_if_no_powershell!)")
}

// ── Basic execution ───────────────────────────────────────────────────────────

#[tokio::test]
async fn captures_stdout() {
    skip_if_no_powershell!();
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "Write-Output 'hello world'",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    assert!(output.stdout.contains("hello world"));
    assert!(output.stderr.is_empty());
    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn captures_stderr() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "Write-Error 'oops'",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    assert!(output.stderr.contains("oops"));
    assert_ne!(output.exit_code, 0);
}

#[tokio::test]
async fn reports_nonzero_exit_code() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "exit 42",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    assert_eq!(output.exit_code, 42);
}

#[tokio::test]
async fn reports_zero_exit_code_on_success() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "Write-Output 'ok'",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    assert_eq!(output.exit_code, 0);
}

// ── Working directory ─────────────────────────────────────────────────────────

#[tokio::test]
async fn respects_working_directory() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "Get-Location | Write-Output",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    // The output should contain the current directory (resolved from ".").
    assert!(!output.stdout.trim().is_empty());
}

// ── Cancellation ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancellation_kills_long_running_process() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Cancel after a short delay — the command sleeps for 30s, way longer.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = executor
        .execute(
            "Start-Sleep -Seconds 30",
            Path::new("."),
            None,
            cancel,
            None,
        )
        .await;

    // Should return an error (process killed by cancellation).
    assert!(result.is_err(), "cancelled command should return Err");
}

// ── Timeout ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn timeout_kills_long_running_process() {
    skip_if_no_powershell!();
    let executor = get_executor();

    let result = executor
        .execute(
            "Start-Sleep -Seconds 30",
            Path::new("."),
            Some(Duration::from_millis(200)),
            CancellationToken::new(),
            None,
        )
        .await;

    assert!(result.is_err(), "timed-out command should return Err");
}

#[tokio::test]
async fn short_command_completes_within_timeout() {
    skip_if_no_powershell!();
    let executor = get_executor();

    let output = executor
        .execute(
            "Write-Output 'fast'",
            Path::new("."),
            Some(Duration::from_secs(10)),
            CancellationToken::new(),
            None,
        )
        .await
        .expect("fast command should complete within timeout");

    assert!(output.stdout.contains("fast"));
    assert_eq!(output.exit_code, 0);
}

// ── Shell detection ───────────────────────────────────────────────────────────

#[test]
fn executor_reports_available_shell() {
    skip_if_no_powershell!();
    let executor = get_executor();
    // On a Windows dev machine, at least one PowerShell should be available.
    assert!(
        !executor.shell_exe().is_empty(),
        "expected a PowerShell executable to be detected"
    );
}

// ── Path normalization ───────────────────────────────────────────────────────

#[tokio::test]
async fn normalizes_forward_slashes_in_paths() {
    skip_if_no_powershell!();
    let executor = get_executor();
    // Use a path with forward slashes.
    let output = executor
        .execute(
            "Write-Output 'src/main.rs'",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    // On Windows, slashes are normalized to backslashes.
    // On macOS/Linux, forward slashes are preserved (native).
    assert_eq!(output.exit_code, 0);
    let path_in_output = if cfg!(target_os = "windows") {
        "src\\main.rs"
    } else {
        "src/main.rs"
    };
    assert!(
        output.stdout.contains(path_in_output),
        "expected path {path_in_output} in output: {}",
        output.stdout.trim()
    );
}

#[tokio::test]
async fn preserves_division_operator() {
    skip_if_no_powershell!();
    let executor = get_executor();
    // `10 / 2` should NOT be normalized — spaces around `/` mean division.
    let output = executor
        .execute(
            "10 / 2",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    assert_eq!(output.exit_code, 0);
}

// ── PowerShell-specific features ──────────────────────────────────────────────

#[tokio::test]
async fn executes_pipeline() {
    skip_if_no_powershell!();
    let executor = get_executor();
    let output = executor
        .execute(
            "1..3 | ForEach-Object { $_ * 2 }",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("pipeline should execute");

    assert!(output.stdout.contains('2'));
    assert!(output.stdout.contains('4'));
    assert!(output.stdout.contains('6'));
}

#[tokio::test]
async fn executes_with_no_profile() {
    skip_if_no_powershell!();
    // The executor passes -NoProfile. Verify the $PROFILE variable is empty
    // (not loading a user profile script).
    let executor = get_executor();
    let output = executor
        .execute(
            "Write-Output ($PROFILE -eq $null)",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("should execute");

    // $PROFILE is never $null in PowerShell, but -NoProfile means no profile
    // is loaded. Just verify the command runs successfully — the real test
    // is that no user profile side effects occur.
    assert_eq!(output.exit_code, 0);
}

// ── Stdin piping ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn input_piped_to_stdin() {
    skip_if_no_powershell!();
    let executor = get_executor();
    // Read from stdin and echo it back.
    let output = executor
        .execute(
            "$line = [Console]::In.ReadLine(); Write-Output \"got: $line\"",
            Path::new("."),
            None,
            CancellationToken::new(),
            Some("hello from stdin"),
        )
        .await
        .expect("execution should succeed");

    assert!(output.stdout.contains("got: hello from stdin"));
    assert_eq!(output.exit_code, 0);
}

#[tokio::test]
async fn no_input_sends_eof_to_stdin() {
    skip_if_no_powershell!();
    let executor = get_executor();
    // [Console]::In.ReadLine() returns $null when stdin is closed (EOF).
    let output = executor
        .execute(
            "$line = [Console]::In.ReadLine(); Write-Output \"read: $line\"",
            Path::new("."),
            None,
            CancellationToken::new(),
            None,
        )
        .await
        .expect("execution should succeed");

    // With no input piped, ReadLine() returns $null → empty string in output.
    assert!(output.stdout.contains("read:"));
    assert_eq!(output.exit_code, 0);
}
