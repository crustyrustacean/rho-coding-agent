# Task 3: `ListDir` Tool — Completion Report

**Date:** 2026-04-29  
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Added `ListDir` — a `.gitignore`-aware directory listing tool that uses the `ignore` crate (from ripgrep) for walking.

### New tool: `ListDir` in `rho-tools/src/files.rs`

**Parameters:**
| Parameter | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | no | `"."` | Directory to list (relative to project root) |
| `recursive` | boolean | no | `false` | Walk subdirectories |

**Behaviour:**
- Uses `ignore::WalkBuilder` for `.gitignore`-aware walking
- Respects `.gitignore`, global gitignore, `.git/info/exclude`, and `.ignore`
- Shows hidden/dot files (e.g. `.agents.md`) — `hidden(false)`
- Works without a git repo — `require_git(false)`
- Deterministic ordering via `sort_by_file_name`
- Directories are marked with a trailing `/`
- Empty directories show `(empty directory)`
- Read errors are counted and reported at the end
- Relative paths are resolved against the sandbox root before validation

**Security:**
- Sandbox-validated: all paths checked against `SandboxRoot`
- `ToolRisk::Read` — auto-approved by default policy
- Cancellation token checked before walking and per-entry

### New dependency: `ignore = "0.4"` in `rho-tools/Cargo.toml`

The `ignore` crate is the standard Rust `.gitignore` walker, maintained by the ripgrep author. It correctly handles negation patterns, nested `.gitignore` files, and the full gitignore specification — something that would take ~200+ lines to implement correctly from scratch.

---

## Test Coverage

### Unit tests (in `files.rs`): 4 tests
All for the `truncate_for_error` helper (shared with `EditFile`).

### Integration tests (in `tool_tests.rs`): 12 new tests

| Test | What it verifies |
|---|---|
| `list_dir_lists_files_in_root` | Files appear in output |
| `list_dir_marks_directories_with_trailing_slash` | Directories have `/` suffix; non-recursive doesn't list nested files |
| `list_dir_recursive_lists_nested_files` | Recursive mode lists deeply nested files |
| `list_dir_respects_gitignore` | `*.log` ignored but `.gitignore` and tracked files appear |
| `list_dir_shows_hidden_files` | Dotfiles appear (hidden(false)) |
| `list_dir_subdirectory` | Listing a subdirectory works |
| `list_dir_empty_directory` | Shows "(empty directory)" message |
| `list_dir_rejects_path_outside_sandbox` | Sandbox validation enforced |
| `list_dir_not_a_directory_returns_error` | Pointing at a file returns error result |
| `list_dir_is_risk_read` | Risk classification correct |
| `list_dir_respects_cancellation` | Cancelled token returns error result |

---

## File Changes

| File | Change |
|---|---|
| `rho-tools/src/files.rs` | Added `ListDir` struct and `Tool` impl |
| `rho-tools/src/lib.rs` | Export `ListDir`, register in `register_all` |
| `rho-tools/Cargo.toml` | Added `ignore = "0.4"` dependency |
| `rho-tools/tests/tool_tests.rs` | Added 12 `ListDir` integration tests |
| `AGENTS.md` | Updated project layout and key types table |

---

## Design Decisions

1. **`ignore` crate over hand-rolled** — `.gitignore` semantics are complex (negation patterns, nested files, global config). The `ignore` crate handles all of this correctly and is battle-tested via ripgrep. Writing it ourselves would be ~200 lines of subtle pattern matching with edge-case bugs.

2. **Hidden files shown by default** — Project context files like `.agents.md`, `.cursorrules`, and `.rho/prompt.md` are dotfiles. Hiding them by default would make the tool useless for the agent's primary use case.

3. **Relative path resolution** — Paths like `"src"` are resolved against the sandbox root before canonicalization. This allows the model to use short relative paths rather than absolute canonical paths.

4. **Trailing `/` on directories** — Makes it easy for the model to distinguish files from directories in the flat text output.

5. **Non-recursive by default** — Recursive walks of large projects can produce enormous output. The model must explicitly opt in.

6. **Error tolerance** — Individual entry read errors are counted and reported rather than aborting the entire walk. Some entries may be locked or inaccessible on Windows.
