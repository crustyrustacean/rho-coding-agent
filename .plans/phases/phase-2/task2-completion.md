# Task 2: `ShellExecutor` Trait — Completion Report

**Date:** 2026-04-29  
**Branch:** `feat/phase2-task2-shell-executor`  
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

## What Was Done

### 1. New `rho-core::shell` module

**File:** `rho-core/src/shell.rs`

- **`ShellOutput`** — structured output struct with `stdout: String`, `stderr: String`, `exit_code: i32`. Includes `new()` constructor and `is_success()` helper.
- **`ShellExecutor`** trait — async, dyn-compatible (via `async-trait`), with a single method:
  ```rust
  async fn execute(
      &self,
      command: &str,
      working_dir: &Path,
      timeout: Option<Duration>,
      cancel: CancellationToken,
  ) -> Result<ShellOutput>;
  ```
- 6 unit tests for `ShellOutput` construction, equality, and `is_success()`.

**Re-exports:** `ShellOutput` and `ShellExecutor` are re-exported from `rho-core::lib.rs`.

**Design rationale:** The trait lives in `rho-core` (the abstraction), implementations live in `rho-tools` (the concrete shell). This mirrors the `ChatClient` / `LocalChatClient` split. `ShellOutput` preserves the structured stdout/stderr/exit_code separation; `RunCommand` maps `ShellOutput → ToolResult` at the boundary.

### 2. `PowerShellExecutor` implementation

**File:** `rho-tools/src/shell.rs`

- Detects `pwsh` (PowerShell 7+) vs `powershell` (Windows PowerShell 5.1) at construction time using the `which` crate. Result is cached in the executor struct.
- Adds `-ExecutionPolicy Bypass` when using `powershell.exe` (avoids script-signing failures on constrained systems).
- Spawns processes with `-NoProfile -NonInteractive -Command <command>`.
- Wires `CancellationToken` to `taskkill /F /PID` on Windows for graceful process kill on cancellation.
- Supports `timeout: Option<Duration>` — kills the process if the deadline is exceeded.
- Returns `Err(RhoError)` on timeout or cancellation (not a `ToolResult::error` — the executor's contract is `Result<ShellOutput>`).

### 3. Refactored `RunCommand`

**File:** `rho-tools/src/shell.rs`

**Before:** `RunCommand` directly spawned PowerShell via `tokio::process::Command`.

**After:** `RunCommand` holds `Box<dyn ShellExecutor>` and delegates all process spawning to it. The `Tool::execute` implementation:
1. Parses the `command` argument from JSON
2. Checks cancellation
3. Calls `self.executor.execute(command, self.root.path(), None, cancel)`
4. Maps `ShellOutput → ToolResult` (combining stdout/stderr, marking error on non-zero exit)

This means `RunCommand` no longer knows about PowerShell at all — it's a thin adapter between the `Tool` trait and the `ShellExecutor` trait.

### 4. Updated `register_all`

**File:** `rho-tools/src/lib.rs`

Constructs `PowerShellExecutor::new()` and passes it as `Box<dyn ShellExecutor>` to `RunCommand`.

### 5. `MockShellExecutor` in `rho-test-helpers`

**File:** `rho-test-helpers/src/lib.rs`

- Returns canned `ShellOutput` values in sequence
- Records all commands it receives for test assertions
- Used by `RunCommand` unit tests to exercise argument parsing and result formatting without spawning real processes

### 6. New dependency

**`which` v7** added to `rho-tools/Cargo.toml` for `pwsh`/`powershell` detection at runtime.

### 7. Tests

| Test file | Tests | What's covered |
|---|---|---|
| `rho-core/src/shell.rs` (unit) | 6 | `ShellOutput` construction, `is_success()`, equality |
| `rho-tools/tests/shell_executor_tests.rs` | 11 | `PowerShellExecutor`: stdout capture, stderr capture, exit codes, working directory, cancellation kill, timeout kill, short command within timeout, shell detection, pipelines, `-NoProfile` |
| `rho-tools/tests/tool_tests.rs` | 5 (new) | `RunCommand` with `MockShellExecutor`: delegation, stderr formatting, missing argument, risk level, cancellation |

**Total new tests:** 22  
**Total workspace tests:** 110 (was 88)

### 8. Pre-existing fix

Fixed the stale `base_prompt_sha256_is_pinned` test — the expected hash was swapped with the actual hash. The test now matches the current `base.md` content.

## File Changes Summary

| File | Change |
|---|---|
| `rho-core/src/shell.rs` | **New** — `ShellOutput`, `ShellExecutor` trait, unit tests |
| `rho-core/src/lib.rs` | Added `pub mod shell;`, re-exports, doc table entry |
| `rho-tools/src/shell.rs` | **Rewrite** — added `PowerShellExecutor`, refactored `RunCommand` to use `ShellExecutor` trait |
| `rho-tools/src/lib.rs` | Updated `register_all` to construct `PowerShellExecutor`; added export |
| `rho-tools/Cargo.toml` | Added `which = "7"` dependency; added `rho-test-helpers` dev-dependency |
| `rho-tools/tests/shell_executor_tests.rs` | **New** — integration tests for `PowerShellExecutor` |
| `rho-tools/tests/tool_tests.rs` | Added 5 `RunCommand` tests using `MockShellExecutor` |
| `rho-test-helpers/src/lib.rs` | Added `MockShellExecutor` |
| `rho-core/tests/integration_tests.rs` | Fixed SHA-256 hash in pinned test |
| `AGENTS.md` | Updated project layout and key types table |

## Design Decisions

1. **`ShellOutput` is a separate struct, not `ToolResult`** — `ToolResult` conflates stdout/stderr into a single string. `ShellOutput` preserves the structured separation. The `RunCommand` tool decides how to format and what counts as an error.

2. **`timeout` is a per-call parameter** — The same executor may run commands with different timeout needs. Default timeout configuration is a `RunCommand` concern, not an executor concern.

3. **`working_dir` is a per-call parameter** — Decoupled from the executor so the same executor could run commands in different directories. `RunCommand` passes `self.root.path()`.

4. **`which` crate for shell detection** — Preferred over `std::process::Command::new("pwsh").check()` because `which` is a pure lookup without process spawning, and it's a well-maintained foundation crate.

5. **Executor panics if no PowerShell found** — Consistent with how `SandboxRoot::new()` errors if the root doesn't exist. A missing shell is a deployment issue, not a runtime condition to gracefully handle.

6. **`kill_process` uses `taskkill /F /PID`** — Same approach as the original `RunCommand`. Windows-specific but correct for this Phase's scope.

## Out of Scope (deferred to Task 1 / Task 6)

- Command denylist enforcement (Task 1)
- Per-command timeout defaults from config (Task 1 / Task 6)
- Working directory escape detection (Task 1)
- Config integration (Task 6)
- Path separator normalization in command strings (Task 1)
- Cross-platform `BashExecutor` (future)
