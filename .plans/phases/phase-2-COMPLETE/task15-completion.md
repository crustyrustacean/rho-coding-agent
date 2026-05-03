# Task 15: Test Suite Audit — Completion Report

**Date:** 2026-05-01
**Status:** ✅ Complete — all tests pass, CI green

---

## What Was Done

Audited the test suite for coverage, deduplication, isolation, and maintainability. Promoted shared infrastructure into `rho-test-helpers` and verified all audit criteria.

### 1. Promoted `FileTestEnv` into `rho-test-helpers`

**`rho-test-helpers/src/lib.rs`:**
- Added `FileTestEnv` — a convenience wrapper around `(TempDir, SandboxRoot)` with methods for creating files, directories, reading files, and checking existence inside the sandbox
- `FileTestEnv::write_file(relative_path, contents)` — creates a file with auto-created parent directories
- `FileTestEnv::create_dir(relative_path)` — creates a subdirectory
- `FileTestEnv::read_file(relative_path)` — reads a file
- `FileTestEnv::exists(relative_path)` — checks existence
- `FileTestEnv::root()`, `sandbox()`, `temp_dir()` — accessors
- Implements `Default` for ergonomic construction

**Backward compatibility:**
- `tempdir_with_sandbox()` now delegates to `FileTestEnv::new()` internally, so the helper is a single source of truth
- Existing tests using `setup()` → `tempdir_with_sandbox()` continue to work unchanged

### 2. Added `detect_shell()` to `rho-test-helpers`

**`rho-test-helpers/src/lib.rs`:**
- `detect_shell()` returns `Some("pwsh")` if PowerShell 7+ is on PATH, `Some("powershell")` if Windows PowerShell is available, or `None` if neither is found
- Mirrors the private `detect_powershell()` in `rho-tools` but returns `Option` instead of panicking
- Allows integration tests to skip gracefully when no PowerShell is available (e.g., CI runners without `pwsh`)
- Uses `which` crate (same as `rho-tools`)

**New dependency:** `which = "7"` in `rho-test-helpers/Cargo.toml`

### 3. Updated `setup()` in `tool_tests.rs`

**`rho-tools/tests/tool_tests.rs`:**
- `setup()` now delegates to `rho_test_helpers::tempdir_with_sandbox()` instead of duplicating the tempdir + sandbox construction
- Single source of truth for sandbox creation logic

### 4. Verified EditFile test coverage

All four required scenarios are covered:

| Scenario | Test |
|---|---|
| Exact match | `edit_file_single_replacement` |
| Ambiguous match | `edit_file_ambiguous_match_returns_error` |
| No match | `edit_file_old_text_not_found_returns_error` |
| Overlapping edits | `edit_file_overlapping_edits_return_error` |

Additional EditFile tests: multiple non-overlapping edits, empty edits array, missing path, outside sandbox, file not found, deletion with empty new text, risk classification, cancellation.

### 5. Verified JSON fixture coverage

All 8 fixtures are non-overlapping and serve distinct test scenarios:

| Fixture | Purpose |
|---|---|
| `chat_completion.json` | Basic text completion (Phase 1) |
| `tool_call.json` | Single tool call (Phase 1) |
| `multi_tool_call.json` | Multiple tool calls in one response |
| `tool_call_with_content.json` | Mixed content + tool calls |
| `write_tool_call.json` | Write tool with complex JSON arguments |
| `edit_tool_call.json` | Edit tool with array of edits |
| `finish_reason_length.json` | Token limit reached |
| `finish_reason_content_filter.json` | Content filter triggered |

No overlap with Phase 1 fixtures. No consolidation needed.

### 6. Verified config loading test coverage

All three required scenarios are covered:

| Scenario | Test |
|---|---|
| Missing files | `load_with_no_files_returns_defaults` |
| Malformed TOML | `malformed_toml_returns_error` |
| Unknown keys | `unknown_keys_are_ignored` |

Additional config tests: project overrides user, user-only config, partial agent config, single-section config, API key resolution, sandbox toggle, redaction toggle, token budget, shell denied commands.

### 7. Verified security test isolation

All security tests are deterministic and isolated:

- **No real network calls** — Egress tests use `check_egress()` directly (unit) or endpoints that fail at the HTTP level with 5-second timeouts (integration). No assertions depend on network responses.
- **No real credential store access** — API key tests verify env var *names*, not values.
- **No real file system mutation** — Sandbox tests use `tempfile::TempDir` for cleanup.
- **No process spawning for denylist** — `MockShellExecutor` records zero calls for denied commands.

---

## Test Coverage Summary

| Suite | Tests |
|---|---|
| rho (binary) unit | 6 |
| rho-core unit | 109 |
| rho-core integration | 40 |
| rho-core security | 34 |
| rho-tools unit | 20 |
| rho-tools shell executor | 13 |
| rho-tools tool | 58 |
| **Total** | **280** |

---

## File Changes

| File | Change |
|---|---|
| `rho-test-helpers/src/lib.rs` | Added `FileTestEnv` struct with `write_file`, `create_dir`, `read_file`, `exists`, `root`, `sandbox`, `temp_dir`. Added `detect_shell()`, `which_exists()`. Refactored `tempdir_with_sandbox()` to delegate to `FileTestEnv::new()`. |
| `rho-test-helpers/Cargo.toml` | Added `which = "7"` dependency |
| `rho-tools/tests/tool_tests.rs` | `setup()` delegates to `rho_test_helpers::tempdir_with_sandbox()`. Added doc comment. |
| `rho-tools/tests/shell_executor_tests.rs` | Updated module doc to mention graceful skip via `detect_shell()`. |

---

## Design Decisions

1. **`FileTestEnv` as a struct, not tuples** — The `(TempDir, SandboxRoot)` tuple pattern is repeated throughout the test suite. `FileTestEnv` bundles them together with convenience methods, making test setup more readable and less error-prone.

2. **`detect_shell()` returns `Option`, not panic** — The `PowerShellExecutor::new()` constructor panics when no shell is found. This is correct for production (a missing shell is a deployment error) but wrong for tests, which should be skippable. `detect_shell()` returns `None` so tests can use `if detect_shell().is_none() { return; }` or similar patterns.

3. **`tempdir_with_sandbox()` preserved for backward compat** — Rather than rewriting all 23 test functions that use `setup()`, I kept `setup()` as a thin wrapper around `tempdir_with_sandbox()`, which itself delegates to `FileTestEnv::new()`. This gives a single source of truth while avoiding a large mechanical refactoring.

4. **`which` crate in `rho-test-helpers`** — Same crate used by `rho-tools` for shell detection. Mirrors the `which_exists()` function without creating a circular dependency.

5. **No fixture consolidation needed** — All 8 JSON fixtures serve distinct scenarios with no overlap. Phase 1 fixtures (`chat_completion.json`, `tool_call.json`) test basic deserialization; Phase 2 fixtures test multi-tool-call, mixed content, write/edit arguments, and finish reasons.
