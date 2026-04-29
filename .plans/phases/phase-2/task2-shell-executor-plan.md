# Task 2: `ShellExecutor` Trait — Implementation Plan

## Goal

Abstract shell execution behind a trait in `rho-core` so that `RunCommand` (in `rho-tools`) depends on the trait, not on PowerShell directly. `PowerShellExecutor` is the first (and default) implementation. This is the architectural prerequisite for Task 1 (expanding `RunCommand` with denylist, timeout, `pwsh` detection, etc.).

## Design Decisions

### 1. Where does the trait live?

The trait lives in a new `rho-core::shell` module.

**Rationale:** The roadmap says "rho-core does not know about PowerShell, shells, or any specific command execution." The `ShellExecutor` trait is the *abstraction* — it defines the interface without depending on any shell. The concrete `PowerShellExecutor` lives in `rho-tools`, where PowerShell knowledge belongs. This mirrors the `ChatClient` / `LocalChatClient` split: trait in core, implementation in a downstream crate.

**Module name:** `shell` — clear, short, and distinct from `tool`. Future `ShellExecutor` variants (`BashExecutor`, etc.) also implement this trait.

### 2. What does the trait look like?

```rust
/// Output captured from a shell command execution.
#[derive(Clone, Debug)]
pub struct ShellOutput {
    /// The command's standard output.
    pub stdout: String,
    /// The command's standard error.
    pub stderr: String,
    /// The process exit code (0 = success).
    pub exit_code: i32,
}

/// The interface for executing shell commands.
///
/// Implementations target specific shells (PowerShell, Bash, etc.).
/// The trait lives in `rho-core` so the tool layer depends on the
/// abstraction, not on any concrete shell.
#[async_trait]
pub trait ShellExecutor: Send + Sync {
    /// Execute `command` in `working_dir` and return the captured output.
    ///
    /// If `timeout` is `Some(duration)`, the process is killed if it
    /// exceeds the deadline and an error result is returned.
    ///
    /// `cancel` is checked at spawn time and during the wait; if cancelled,
    /// the child process is killed and an error result is returned.
    async fn execute(
        &self,
        command: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Result<ShellOutput>;
}
```

**Why `ShellOutput` is a struct, not `ToolResult`:** `ToolResult` conflates stdout/stderr into a single `output: String` with an `is_error` bool. `ShellOutput` preserves the structured stdout/stderr/exit_code separation that `RunCommand` currently assembles ad-hoc. The `RunCommand` tool maps `ShellOutput → ToolResult` at the boundary — it decides how to format the combined output string and what counts as an error. This keeps the executor's contract clean and the tool's presentation logic separate.

**Why `timeout` is a parameter, not constructor config:** The same executor may run different commands with different timeout needs (e.g., a quick `Get-ChildItem` vs. a long `cargo build`). Passing it per-call is more flexible. If all commands share a default, that's a `RunCommand` concern, not an executor concern.

**Why `working_dir` is a parameter:** The executor is a reusable service; the sandbox root is a `RunCommand` property. Decoupling them means the same executor could run commands in different directories (e.g., a future tool that runs git commands in a subdirectory). The `RunCommand` tool passes `self.root.path()` as `working_dir`.

### 3. Where does `PowerShellExecutor` live?

In `rho-tools::shell`, as a sibling to `RunCommand`.

**Rationale:** `rho-core` must not depend on `tokio::process` or know about `pwsh`/`powershell`. The executor implementation is shell-specific infrastructure that belongs in `rho-tools`.

**What it does:**
- Detect `pwsh` vs `powershell` availability (the current `powershell_exe()` function is a stub that always returns `"pwsh"` — this gets real detection).
- Set appropriate execution policy flags (`-ExecutionPolicy Bypass` on Windows when `powershell.exe` is used, to avoid script-signing failures).
- Normalize path separators in the command string (replace forward slashes with backslashes on Windows).
- Spawn the process with `-NoProfile -NonInteractive -Command <command>`.
- Wire `CancellationToken` to process kill (`taskkill /F /PID` on Windows).
- Respect the `timeout` parameter using `tokio::time::timeout`.

### 4. How does `RunCommand` change?

**Before (current):**
```
RunCommand { root: SandboxRoot }
  → directly spawns PowerShell via tokio::process::Command
  → manually formats stdout/stderr/exit_code into ToolResult
```

**After:**
```
RunCommand { root: SandboxRoot, executor: Box<dyn ShellExecutor> }
  → delegates to executor.execute(command, root.path(), timeout, cancel)
  → maps ShellOutput → ToolResult
```

The `Tool::execute` implementation becomes a thin adapter: parse arguments, call the executor, format the result. All process-spawning logic moves into `PowerShellExecutor`.

### 5. How does `register_all` change?

```rust
pub fn register_all(registry: &mut ToolRegistry, root: SandboxRoot) {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    let executor = Box::new(PowerShellExecutor::new());
    registry.register(Box::new(RunCommand { root, executor }));
}
```

`PowerShellExecutor::new()` does the `pwsh`/`powershell` detection at construction time (not per-command). The result is cached in the executor struct.

### 6. Re-exports from `rho-core`

`ShellOutput` and `ShellExecutor` are re-exported from `rho-core`'s lib.rs alongside other traits, so downstream crates can depend on the trait without reaching into the module.

## File Changes

### New files

| File | Contents |
|---|---|
| `rho-core/src/shell.rs` | `ShellOutput` struct, `ShellExecutor` trait |
| `rho-tools/tests/shell_executor_tests.rs` | Integration tests for `PowerShellExecutor` |

### Modified files

| File | Change |
|---|---|
| `rho-core/src/lib.rs` | Add `pub mod shell;` + re-exports for `ShellOutput`, `ShellExecutor` |
| `rho-tools/src/shell.rs` | Add `PowerShellExecutor` struct; refactor `RunCommand` to use `ShellExecutor` trait instead of direct `tokio::process::Command` |
| `rho-tools/src/lib.rs` | Export `PowerShellExecutor`; update `register_all` to construct it |
| `rho-test-helpers/src/lib.rs` | Add `MockShellExecutor` (canned output, records commands) |

## Test Plan

### Unit tests (in `rho-core/src/shell.rs`)

- `ShellOutput` construction and field access
- These are minimal — the struct is a plain data type

### Integration tests (in `rho-tools/tests/shell_executor_tests.rs`)

- `PowerShellExecutor` runs a simple command and captures stdout
- `PowerShellExecutor` captures stderr from a failing command
- `PowerShellExecutor` reports the correct exit code
- `PowerShellExecutor` respects `CancellationToken` (cancel before completion)
- `PowerShellExecutor` respects timeout (command sleeps, timeout fires)
- `pwsh` vs `powershell` detection (at least documents the expected behaviour)

### Existing test impact

- `rho-tools/tests/tool_tests.rs` — no `RunCommand` tests exist yet, so no breakage
- `rho-core/tests/integration_tests.rs` — no changes needed (agent loop doesn't touch shell directly)
- `rho-core/tests/security_tests.rs` — no changes needed yet (denylist tests come in Task 1)

### `MockShellExecutor` in `rho-test-helpers`

A test double that returns canned `ShellOutput` values and records all commands it received. Used by future `RunCommand` unit tests that need to exercise the tool's argument parsing and result formatting without spawning a real shell.

```rust
pub struct MockShellExecutor {
    outputs: Arc<Mutex<Vec<ShellOutput>>>,
    commands: Arc<Mutex<Vec<String>>>,
}
```

## Implementation Order

1. **Create `rho-core/src/shell.rs`** — `ShellOutput` struct + `ShellExecutor` trait
2. **Update `rho-core/src/lib.rs`** — add module + re-exports
3. **Implement `PowerShellExecutor` in `rho-tools/src/shell.rs`** — move all process-spawning logic out of `RunCommand` into the new executor; add `pwsh`/`powershell` detection, execution policy flags, path normalization, timeout support
4. **Refactor `RunCommand`** — accept `Box<dyn ShellExecutor>`, delegate to it, map `ShellOutput → ToolResult`
5. **Update `rho-tools/src/lib.rs`** — export `PowerShellExecutor`, update `register_all`
6. **Add `MockShellExecutor` to `rho-test-helpers`**
7. **Write integration tests** for `PowerShellExecutor`
8. **Run `cargo xtask ci`** — confirm fmt, lint, build, and all tests pass

## Out of Scope (deferred to Task 1)

- Command denylist enforcement
- Per-command timeout defaults from config
- Working directory escape detection (`cd` outside sandbox)
- Config integration (denylist comes from config in Task 6)

These are all `RunCommand`-level concerns that build *on top of* the `ShellExecutor` abstraction. The trait's job is just: run a command, capture output, respect timeout and cancellation.
