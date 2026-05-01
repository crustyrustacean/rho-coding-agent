# Task 11: Deserialization Tests for Tool-Call Responses — Completion Report

**Date:** 2026-05-01 (retroactive)
**Branch:** Commit `815455c`
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Added comprehensive JSON fixture tests for tool-call response deserialization, covering multi-tool-call parsing, mixed content+tool_calls, write/edit argument validation, all `FinishReason` variants, and round-trip serialization.

### New JSON fixtures

Six new fixture files in `rho-core/tests/fixtures/responses/`:

| Fixture | What it models |
|---|---|
| `multi_tool_call.json` | Two tool calls in one response (read_file + list_dir) |
| `tool_call_with_content.json` | Both text content and tool_calls in one message |
| `write_tool_call.json` | Write tool with complex JSON arguments (nested strings, newlines) |
| `edit_tool_call.json` | Edit tool with array of edits in arguments |
| `finish_reason_length.json` | Response truncated due to max tokens |
| `finish_reason_content_filter.json` | Response blocked by content filter |

### New deserialization tests

Eight tests in `rho-core/tests/integration_tests.rs`:

| Test | What it verifies |
|---|---|
| `fixture_multi_tool_call_deserializes` | Two tool calls parse correctly; names, IDs, and arguments all accessible |
| `fixture_tool_call_with_content_deserializes` | Both `content` and `tool_calls` fields populated |
| `fixture_write_tool_call_deserializes` | Write tool arguments are valid JSON with expected fields |
| `fixture_edit_tool_call_deserializes` | Edit tool arguments contain edits array with old_text/new_text |
| `fixture_finish_reason_length_deserializes` | `finish_reason: "length"` maps to `FinishReason::Length` |
| `fixture_finish_reason_content_filter_deserializes` | `finish_reason: "content_filter"` maps to `FinishReason::ContentFilter` |
| `all_fixture_finish_reasons_round_trip` | All known `FinishReason` variants survive serialize + deserialize |

### `FinishReason` improvements

- `FinishReason` now derives `PartialEq` and `Eq` (needed for the round-trip test)
- Added custom `Deserialize` impl that maps unknown finish reasons to `FinishReason::Other(String)` instead of failing — non-standard providers (llama.cpp, vLLM, etc.) may return values not in the OpenAI spec

---

## Test Coverage

### New tests (8)

All in `rho-core/tests/integration_tests.rs`, under the "Task 11: expanded deserialization tests" section.

### Fixture coverage summary

| Fixture | From Phase | Tests using it |
|---|---|---|
| `chat_completion.json` | Phase 1a | `fixture_chat_completion_deserializes` |
| `tool_call.json` | Phase 1a | `fixture_tool_call_deserializes` |
| `multi_tool_call.json` | Phase 2, Task 11 | `fixture_multi_tool_call_deserializes` |
| `tool_call_with_content.json` | Phase 2, Task 11 | `fixture_tool_call_with_content_deserializes` |
| `write_tool_call.json` | Phase 2, Task 11 | `fixture_write_tool_call_deserializes` |
| `edit_tool_call.json` | Phase 2, Task 11 | `fixture_edit_tool_call_deserializes` |
| `finish_reason_length.json` | Phase 2, Task 11 | `fixture_finish_reason_length_deserializes` |
| `finish_reason_content_filter.json` | Phase 2, Task 11 | `fixture_finish_reason_content_filter_deserializes` |

---

## File Changes

| File | Change |
|---|---|
| `rho-core/tests/fixtures/responses/multi_tool_call.json` | **New** — fixture |
| `rho-core/tests/fixtures/responses/tool_call_with_content.json` | **New** — fixture |
| `rho-core/tests/fixtures/responses/write_tool_call.json` | **New** — fixture |
| `rho-core/tests/fixtures/responses/edit_tool_call.json` | **New** — fixture |
| `rho-core/tests/fixtures/responses/finish_reason_length.json` | **New** — fixture |
| `rho-core/tests/fixtures/responses/finish_reason_content_filter.json` | **New** — fixture |
| `rho-core/tests/integration_tests.rs` | Added 8 deserialization tests |
| `rho-core/src/response.rs` | Added `FinishReason::Other(String)` variant with custom `Deserialize` impl |

---

## Design Decisions

1. **`FinishReason::Other` for unknown values** — Non-standard providers (llama.cpp, vLLM, Ollama) may return finish reasons not in the OpenAI spec. Rather than failing deserialization, unknown values are captured as `Other(String)`. This makes the agent resilient to provider variations.

2. **Custom `Deserialize` instead of `#[serde(other)]`** — The `#[serde(other)]` attribute doesn't capture the unknown value. A custom impl maps known strings to variants and captures unknown strings in `Other`.

3. **`Serialize` derive only, custom `Deserialize`** — `FinishReason` derives `Serialize` (produces standard snake_case) but has a hand-written `Deserialize` that handles unknown values. This asymmetry is intentional: outgoing serialization should be spec-compliant; incoming deserialization should be tolerant.

4. **Fixtures test real JSON shapes** — Each fixture is a realistic model API response, not a minimal stub. This catches edge cases like null `logprobs`, missing optional fields, and nested JSON in `arguments` strings.

5. **Round-trip test for all variants** — The `all_fixture_finish_reasons_round_trip` test ensures that serialization + deserialization is an identity operation for all known `FinishReason` variants. This guards against accidental breakage when adding new variants.
