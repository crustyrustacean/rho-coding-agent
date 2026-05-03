# Task 1: Expand `RunCommand` — Completion Report

**Date:** 2026-04-29  
**Branch:** `feat/phase2-task2-shell-executor` (Task 2 branch reused — Task 1 is a continuation)  
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

## What Was Done

Task 1 sub-items and their status:

| Sub-item | Status | Notes |
|---|---|---|
| Detect `pwsh` vs `powershell` availability | ✅ Done in Task 2 | `PowerShellExecutor::new()` with `which` crate |
| Set appropriate execution policy flags | ✅ Done in Task 2 | `-ExecutionPolicy Bypass` for `powershell.exe` |
| Capture and return structured output | ✅ Done in Task 2 | `ShellOutput` struct with stdout/stderr/exit_code |
| Timeout support | ✅ Done in Task 2 | `timeout: Option<Duration>` parameter on `ShellExecutor::execute` |
| Normalize path separators | ✅ Done in Task 1 | `normalize_path_separators()` in `PowerShellExecutor` |
| **Command denylist** | ✅ Done in Task 1 | `CommandDenylist` struct with default PowerShell set |
| **Working directory flag** | ✅ Done in Task 1 | Warning on `cd ..` / `Set-Location ..` patterns |

### 1. Command Denylist

**File:** `rho-tools/src/shell.rs`

`CommandDenylist` enforces a list of dangerous commands that must not be executed. Two matching strategies:

- **Command name denylist** — The first token of the command is checked case-insensitively against a list of denied names.
- **Flag combination denylist** — If ALL flags in a combination are present in the command, it is denied.

Default PowerShell denylist:

| Denied command | Why |
|---|---|
| `Remove-Item` | File deletion |
| `Invoke-WebRequest` | Network egress / data exfiltration |
| `Invoke-RestMethod` | Network egress / data exfiltration |
| `Start-Process` | Arbitrary process launch |
| `New-Service` | System modification |
| `Set-ExecutionPolicy` | Security bypass |

| Denied flag combination | Why |
|---|---|
| `-Recurse` + `-Force` | Recursive force-delete is extremely destructive |

**Integration:** `RunCommand` checks the denylist **before** delegating to the executor. If denied, the tool returns a `ToolResult::error` with the denial reason — the executor is never called. This means the `MockShellExecutor` in tests records zero calls for denied commands.

**Config integration** (Task 6) will allow customising the denylist via `.rho/config.toml`. The `CommandDenylist` struct is designed for this: it's a plain data struct that can be constructed from config.

### 2. Path Separator Normalization

**File:** `rho-tools/src/shell.rs`

`normalize_path_separators()` converts forward slashes to backslashes in path-like contexts before PowerShell execution. The heuristic: replace `/` with `\` when the slash is adjacent to a "path character" (alphanumeric, `.`, `_`, or `-`). This handles:

- `src/main.rs` → `src\main.rs`
- `C:/Users/foo` → `C:\Users\foo`
- `src/lib/mod.rs` → `src\lib\mod.rs`

But preserves:
- `10 / 2` (division with spaces around `/`)
- `1 / 2` (standalone slash surrounded by spaces)

**Integration:** Applied inside `PowerShellExecutor::execute()` before building the argument list. This is a PowerShell-specific concern, so it lives in the executor, not in the `ShellExecutor` trait.

### 3. Working Directory Escape Detection

**File:** `rho-tools/src/shell.rs`

`command_attempts_directory_escape()` detects obvious attempts to navigate outside the project directory via `cd ..` or `Set-Location ..`. This is a **warning**, not a denial — the command still executes, but a `[WARNING: command may navigate outside project directory]` prefix is added to the output.

This is a best-effort heuristic:
- Catches: `cd ..`, `cd ../other`, `Set-Location ..`
- Misses: `Push-Location ..`, `cd $env:TEMP`, environment variable expansion, etc.

The approval gate is the primary defense. This warning reduces accidental escapes, not determined ones.

### 4. Updated `register_all`

**File:** `rho-tools/src/lib.rs`

Constructs `CommandDenylist::default_powershell()` and passes it to `RunCommand`.

## Tests Added

### Unit tests (in `rho-tools/src/shell.rs`): 20 tests

| Category | Count | Coverage |
|---|---|---|
| CommandDenylist | 5 | default blocks, case insensitivity, safe commands |
| Path normalization | 8 | path slashes, drive colon, division, backslash, mixed, empty, trailing, standalone |
| Working directory escape | 7 | cd .., cd ../other, Set-Location .., case insensitive, cd subdirectory, cd absolute, regular command |

### Integration tests (in `rho-tools/tests/tool_tests.rs`): 18 new tests

| Category | Count | Coverage |
|---|---|---|
| CommandDenylist | 13 | All 6 denied commands, -Recurse -Force combo, -Force -Recurse combo, safe commands, case insensitivity, reason message, -Recurse alone, -Force alone |
| RunCommand + denylist | 2 | Denies blacklisted command (executor never called), allows safe command |
| Working directory warning | 3 | cd .. warning, Set-Location .. warning, cd src no warning |

### Integration tests (in `rho-tools/tests/shell_executor_tests.rs`): 2 new tests

| Category | Coverage |
|---|---|
| Path normalization | Forward slashes in paths normalized in real execution |
| Division preserved | `10 / 2` not broken by normalization |

### Test count progression

| | After Task 2 | After Task 1 |
|---|---|---|
| rho-core unit | 49 | 49 |
| rho-core integration | 15 | 15 |
| rho-core security | 20 | 20 |
| rho-tools unit | 0 | 20 (+20) |
| rho-tools shell executor | 11 | 13 (+2) |
| rho-tools tool | 15 | 33 (+18) |
| **Total** | **110** | **150 (+40)** |

## File Changes

| File | Change |
|---|---|
| `rho-tools/src/shell.rs` | Added `CommandDenylist` struct, `normalize_path_separators()`, `command_attempts_directory_escape()`, `is_path_char()`. Updated `PowerShellExecutor::execute()` to normalize paths. Updated `RunCommand` to add `denylist` field, check before execution, and add working directory warning. Added 20 unit tests. |
| `rho-tools/src/lib.rs` | Export `CommandDenylist`. Updated `register_all` to construct denylist. |
| `rho-tools/tests/tool_tests.rs` | Added 18 denylist and working directory tests. Updated existing `RunCommand` tests to include `denylist` field. |
| `rho-tools/tests/shell_executor_tests.rs` | Added 2 path normalization integration tests. |
| `AGENTS.md` | Added `CommandDenylist` to key types table. |

## Design Decisions

1. **Denylist is enforced in `RunCommand`, not in `ShellExecutor`** — The executor is a low-level abstraction that just runs commands. The denylist is a policy decision that belongs at the tool level, where it can be configured per-tool and integrated with the approval system.

2. **Denylist denies, doesn't warn** — A denied command returns `ToolResult::error("command denied: ...")`. The model sees this as an error and can adapt. Warnings would be lost in the output.

3. **Working directory escape is a warning, not a denial** — `cd ..` is common and often stays within the sandbox (e.g., `cd ..` from `project/src/` goes to `project/`). Denying it would be too aggressive. The warning makes the model (and user) aware without blocking legitimate navigation.

4. **Path normalization heuristic** — Adjacent-path-character detection is conservative: it normalizes `src/main.rs` but preserves `10 / 2`. It won't catch all edge cases (e.g., `100/200` as a ratio), but those are rare in coding agent commands and the cost of a false normalization is low.

5. **Flag combination check is order-independent** — `-Recurse -Force` and `-Force -Recurse` are both caught.

6. **Case-insensitive matching** — PowerShell is case-insensitive, so the denylist must be too.

7. **Config integration deferred to Task 6** — The `CommandDenylist` struct is designed to be constructed from config (just a vec of command names and flag combos), but the actual TOML loading and customisation is Task 6.

## Out of Scope (deferred)

- Config-driven denylist customisation (Task 6)
- Denylist for PowerShell aliases (`rm`, `del`, `curl`, `iwr`, etc.) — can be added to the default list or via config in Task 6
- Working directory escape detection for `Push-Location`, environment variable paths, etc.
- Path normalization inside quoted strings only (current approach normalizes all adjacent-path slashes)
- Per-command timeout defaults from config (Task 6)
