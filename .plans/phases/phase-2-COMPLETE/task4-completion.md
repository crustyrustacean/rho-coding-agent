# Task 4: `EditFile` Tool — Completion Report

**Date:** 2026-04-29  
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Added `EditFile` — a targeted exact-match replacement tool that validates edits before applying them.

### New tool: `EditFile` in `rho-tools/src/files.rs`

**Parameters:**
| Parameter | Type | Required | Description |
|---|---|---|---|
| `path` | string | yes | File to edit |
| `edits` | array | yes | List of `{ old_text, new_text }` replacements |

**Validation (applied atomically — all-or-nothing):**
1. Each `old_text` must occur **exactly once** in the file (not ambiguous)
2. Edits must not overlap (sorted by position and checked)
3. Empty `edits` array is an error

**Application:**
- Edits are sorted by position and applied **last-to-first** so earlier byte offsets remain valid
- File is only written if all validations pass
- If any edit fails validation, the file is left unchanged

**Security:**
- Sandbox-validated: all paths checked against `SandboxRoot`
- `ToolRisk::Write` — requires approval by default policy
- Cancellation token checked before read and write
- `old_text` truncated in error messages to avoid dumping large text into the conversation

### Helper: `truncate_for_error`

Truncates text to a configurable length with `…` suffix for error messages. Shared utility in `files.rs` with 4 unit tests.

---

## Test Coverage

### Unit tests (in `files.rs`): 4 tests
For `truncate_for_error`: short text unchanged, long text truncated, exact length boundary, empty string.

### Integration tests (in `tool_tests.rs`): 12 new tests

| Test | What it verifies |
|---|---|
| `edit_file_single_replacement` | One edit applied correctly |
| `edit_file_multiple_non_overlapping_edits` | Two non-overlapping edits applied in one call |
| `edit_file_old_text_not_found_returns_error` | No match → error; file unchanged |
| `edit_file_ambiguous_match_returns_error` | Multiple matches → error; file unchanged |
| `edit_file_overlapping_edits_return_error` | Overlapping regions → error; file unchanged |
| `edit_file_empty_edits_array_returns_error` | Empty array rejected |
| `edit_file_missing_path_returns_error` | Missing `path` argument → Err |
| `edit_file_rejects_path_outside_sandbox` | Sandbox validation enforced |
| `edit_file_is_risk_write` | Risk classification correct |
| `edit_file_respects_cancellation` | Cancelled token returns error result |
| `edit_file_file_not_found_returns_error` | Non-existent file → Err |
| `edit_file_deletion_with_empty_new_text` | Empty `new_text` deletes the matched region |

---

## File Changes

| File | Change |
|---|---|
| `rho-tools/src/files.rs` | Added `EditFile`, `Edit` struct, `truncate_for_error` helper, 4 unit tests |
| `rho-tools/src/lib.rs` | Export `EditFile`, register in `register_all` |
| `rho-tools/tests/tool_tests.rs` | Added 12 `EditFile` integration tests |
| `AGENTS.md` | Updated project layout and key types table |

---

## Design Decisions

1. **Exact-match only** — The model must provide the exact text to find. No regex, no fuzzy matching. This is deterministic and predictable. Tree-sitter node-splitting validation is deferred to Phase 3.

2. **Atomic validation** — All edits are validated before any are applied. If any edit fails, the file is left untouched. This prevents partial-edit corruption.

3. **Unique match required** — `old_text` must occur exactly once. Zero matches is "not found"; two or more is "ambiguous". Both are errors. This prevents the model from accidentally changing the wrong instance.

4. **Last-to-first application** — After sorting edits by position, they are applied in reverse order so that earlier byte offsets remain valid throughout. This is the standard technique for batch string replacement.

5. **Array of edits in one call** — Allows the model to make multiple related changes in a single tool call, which is the common pattern when editing code. Reduces round-trips and maintains consistency.

6. **Truncated error messages** — `old_text` in error messages is truncated to 80 characters to avoid flooding the conversation with large text blocks. The model can always re-read the file to find the correct match.

7. **Empty `new_text` allows deletion** — Setting `new_text` to `""` effectively deletes the matched region. This is intentional and useful for removing lines.
