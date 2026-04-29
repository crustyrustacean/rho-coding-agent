# Phase 2 — Tasks 1 & 2 Combined Summary

**Date:** 2026-04-29  
**Branch:** `feat/phase2-task2-shell-executor`  
**Version:** 0.6.0  
**Status:** ✅ Both tasks complete — all tests pass, fmt + clippy clean

---

## Overview

Tasks 1 and 2 are closely coupled: Task 2 introduces the `ShellExecutor` abstraction that Task 1's `RunCommand` enhancements build on. They were implemented sequentially on the same branch following TDD discipline.

| Task | Title | Status |
|---|---|---|
| 2 | `ShellExecutor` trait in `rho-core` | ✅ Complete |
| 1 | Expand `RunCommand` | ✅ Complete |

---

## Task 2: `ShellExecutor` Trait

### What changed

A new `rho-core::shell` module introduces the shell execution abstraction, separating *what* gets executed from *how* it runs.

**New in `rho-core`:**
- `ShellOutput` — structured output (`stdout`, `stderr`, `exit_code`, `is_success()`)
- `ShellExecutor` trait — async, dyn-compatible interface: `execute(command, working_dir, timeout, cancel) -> Result<ShellOutput>`
- 6 unit tests for `ShellOutput`

**New in `rho-tools`:**
- `PowerShellExecutor` — concrete implementation that:
  - Detects `pwsh` vs `powershell` at construction time (via `which` crate)
  - Adds `-ExecutionPolicy Bypass` for Windows PowerShell
  - Spawns with `-NoProfile -NonInteractive -Command`
  - Wires `CancellationToken` to `taskkill /F /PID`
  - Supports `timeout: Option<Duration>`
- Refactored `RunCommand` — now holds `Box<dyn ShellExecutor>` instead of spawning processes directly

**New in `rho-test-helpers`:**
- `MockShellExecutor` — returns canned `ShellOutput` values, records commands for assertions

**New dependency:** `which = "7"` in `rho-tools`

### Key design decisions

| Decision | Rationale |
|---|---|
| Trait in `rho-core`, implementation in `rho-tools` | Mirrors `ChatClient`/`LocalChatClient` split; `rho-core` must not know about PowerShell |
| `ShellOutput` is separate from `ToolResult` | `ToolResult` conflates stdout/stderr; structured separation lets the tool decide formatting |
| `timeout` and `working_dir` are per-call parameters | Same executor can run different commands with different needs |
| Executor panics if no PowerShell found | Deployment issue, not a runtime condition |

---

## Task 1: Expand `RunCommand`

### What changed

Three security and usability features added to `RunCommand`, building on the `ShellExecutor` abstraction from Task 2.

**1. Command Denylist** (`CommandDenylist`)

Blocks dangerous commands before execution. Two matching strategies:
- **Command name denylist** — first token checked case-insensitively
- **Flag combination denylist** — ALL flags in a combo must be present

Default PowerShell denylist:

| Denied | Why |
|---|---|
| `Remove-Item` | File deletion |
| `Invoke-WebRequest` | Network egress / data exfiltration |
| `Invoke-RestMethod` | Network egress / data exfiltration |
| `Start-Process` | Arbitrary process launch |
| `New-Service` | System modification |
| `Set-ExecutionPolicy` | Security bypass |
| `-Recurse` + `-Force` | Recursive force-delete |

Integration: `RunCommand` checks the denylist **before** delegating to the executor. Denied commands return `ToolResult::error` — the executor is never called.

**2. Path Separator Normalization** (`normalize_path_separators`)

Converts forward slashes to backslashes in path-like contexts before PowerShell execution. Heuristic: replace `/` → `\` when adjacent to a path character (alphanumeric, `.`, `_`, `-`).

| Input | Output | Reason |
|---|---|---|
| `src/main.rs` | `src\main.rs` | Adjacent path chars |
| `C:/Users/foo` | `C:\Users\foo` | Drive-letter path |
| `10 / 2` | `10 / 2` | Spaces around `/` = division |

Applied inside `PowerShellExecutor::execute()`, not in the trait.

**3. Working Directory Escape Detection** (`command_attempts_directory_escape`)

Detects `cd ..` and `Set-Location ..` patterns and adds a `[WARNING]` prefix to the output. This is a warning, not a denial — `cd ..` is often legitimate. The approval gate remains the primary defense.

### Key design decisions

| Decision | Rationale |
|---|---|
| Denylist is enforced in `RunCommand`, not `ShellExecutor` | Policy belongs at the tool level, not the low-level executor |
| Denylist denies, doesn't warn | Model sees an error and adapts; warnings get lost |
| Directory escape is a warning, not a denial | `cd ..` is common and often stays within the sandbox |
| Normalization is heuristic | Conservative: catches `src/main.rs`, preserves `10 / 2` |
| Case-insensitive matching | PowerShell is case-insensitive |
| Config integration deferred to Task 6 | `CommandDenylist` is a plain struct ready for TOML construction |

---

## Test Coverage

| Test suite | Before | After Task 2 | After Task 1 |
|---|---|---|---|
| rho-core unit | 43 | 49 (+6) | 49 |
| rho-core integration | 15 | 15 | 15 |
| rho-core security | 20 | 20 | 20 |
| rho-tools unit | 0 | 0 | 20 (+20) |
| rho-tools shell executor | 0 | 11 (+11) | 13 (+2) |
| rho-tools tool | 10 | 15 (+5) | 33 (+18) |
| **Total** | **88** | **110 (+22)** | **150 (+40)** |

**62 new tests across both tasks.**

---

## All File Changes

| File | Task | Change |
|---|---|---|
| `rho-core/src/shell.rs` | 2 | **New** — `ShellOutput`, `ShellExecutor` trait, 6 unit tests |
| `rho-core/src/lib.rs` | 2 | Added `pub mod shell;`, re-exports, doc table entry |
| `rho-tools/src/shell.rs` | 2+1 | **Rewrite** — `PowerShellExecutor`, refactored `RunCommand`, `CommandDenylist`, `normalize_path_separators()`, `command_attempts_directory_escape()`, 20 unit tests |
| `rho-tools/src/lib.rs` | 2+1 | Export `PowerShellExecutor`, `CommandDenylist`; updated `register_all` |
| `rho-tools/Cargo.toml` | 2 | Added `which = "7"`; added `rho-test-helpers` dev-dependency |
| `rho-tools/tests/shell_executor_tests.rs` | 2+1 | **New** — 13 integration tests for `PowerShellExecutor` |
| `rho-tools/tests/tool_tests.rs` | 2+1 | 33 tests (5 original + 5 Task 2 + 18 Task 1 + 5 pre-existing) |
| `rho-test-helpers/src/lib.rs` | 2 | Added `MockShellExecutor` |
| `rho-core/tests/integration_tests.rs` | pre | Fixed SHA-256 hash in pinned test |
| `AGENTS.md` | 2+1 | Updated project layout, key types table |
| `Cargo.toml` | release | Bumped version to 0.6.0 |

---

## Commits on This Branch

1. `fix(test): correct base_prompt SHA-256 hash in pinned test`
2. `feat(core): add ShellExecutor trait and PowerShellExecutor implementation`
3. `feat(tools): add command denylist, path normalization, and working directory warning`
4. `chore(release): prepare 0.6.0`

---

## Deferred to Later Tasks

| Item | Deferred to |
|---|---|
| Config-driven denylist customisation | Task 6 |
| Denylist for PowerShell aliases (`rm`, `del`, `curl`, `iwr`) | Task 6 |
| Per-command timeout defaults from config | Task 6 |
| Working directory escape for `Push-Location`, env var paths | Future |
| Cross-platform `BashExecutor` | Future |
| Path normalization inside quoted strings only | Future |
