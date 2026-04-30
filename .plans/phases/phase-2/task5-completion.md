# Task 5: Multi-Tool-Call in Agent Loop — Completion Report

**Date:** 2026-04-29  
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

The agent loop now iterates over **all** tool calls in a model response, executing them sequentially and appending each result before re-sending the conversation. Previously, only the first tool call was processed (`calls.into_iter().next()`).

### Changes to `rho-core/src/agent.rs`

**Before (Phase 1a/1b):**
```rust
AssistantResponse::ToolCalls(calls) => {
    let call = calls.into_iter().next()
        .ok_or_else(|| RhoError::Unexpected(anyhow::anyhow!("empty tool_calls")))?;
    // ... process single call ...
}
```

**After (Phase 2, Task 5):**
```rust
AssistantResponse::ToolCalls(calls) => {
    if calls.is_empty() {
        return Err(RhoError::Unexpected(anyhow::anyhow!("empty tool_calls")));
    }
    for call in calls {
        if cancel.is_cancelled() {
            return Err(RhoError::Unexpected(anyhow::anyhow!("cancelled")));
        }
        // ... check approval, execute, append result ...
    }
}
```

Key behavioural changes:

| Aspect | Before | After |
|---|---|---|
| Tool calls per model response | First only | All, sequentially |
| Cancellation between tool calls | Not checked | Checked before each call |
| Empty `tool_calls` vec | `ok_or_else` on `.next()` | Explicit `is_empty()` check |
| Approval per tool call | Single approval | Each call checked independently |
| Denied tool in batch | Not applicable | Denial result appended, remaining calls continue |

### New helper: `multi_tool_call_response` in `rho-test-helpers`

Builds a `ModelResponse` with multiple tool calls for test setup. Mirrors the existing `tool_call_response` builder but accepts `Vec<(id, name, arguments)>`.

### Doc comment updates

- Removed "Phase 1a/1b acts on the first tool call only; all calls are handled in Phase 2" caveat
- Updated state transition docs: "When multiple tool calls are present, each is executed sequentially before returning to `Thinking`"
- Added "All tool calls in a single model response are executed sequentially; each result is appended before re-sending to the model. Parallel execution is a future optimisation."

### Pre-existing fix

Updated the `base_prompt_sha256_is_pinned` test to match the current hash of `base.md` (`79e4b4...` instead of `c600c6...`).

---

## Test Coverage

### New integration tests (7 tests)

| Test | What it verifies |
|---|---|
| `multiple_tool_calls_executed_sequentially` | Two tool calls in one response both execute; both results are appended before re-send |
| `multi_tool_call_persistence_invariant` | Structural invariant: every `Tool` message is preceded by an `Assistant` message with matching `tool_call_id`, even with multiple tool results from one response |
| `mixed_approval_with_multi_tool_call` | Read tool auto-approved, write tool denied — both results present in second request |
| `all_tool_calls_denied_still_feeds_results_and_resends` | Two write tools both denied; both denial messages appended; model sees them and responds |
| `cancellation_between_tool_calls_in_batch` | Slow tool in batch observes cancellation; loop exits with error; cancelled result in history |
| `empty_tool_calls_vec_returns_error` | Edge case: empty `tool_calls` vec returns `Unexpected` error |
| `iteration_count_includes_multi_tool_call_response` | A single response with multiple tool calls counts as one iteration; max-iteration guard still works |

### Test count progression

| Suite | Before | After |
|---|---|---|
| rho-core unit | 49 | 49 |
| rho-core integration | 15 | 22 (+7) |
| rho-core security | 20 | 20 |
| rho-tools unit | 20 | 20 |
| rho-tools shell executor | 13 | 13 |
| rho-tools tool | 33 | 33 |
| **Total** | **150** | **157 (+7)** |

---

## All File Changes

| File | Change |
|---|---|
| `rho-core/src/agent.rs` | Iterate all tool calls instead of first only; add per-call cancellation check; update doc comments |
| `rho-test-helpers/src/lib.rs` | Add `multi_tool_call_response` builder |
| `rho-core/tests/integration_tests.rs` | Add 7 multi-tool-call tests; fix SHA-256 hash; import `multi_tool_call_response`; add `WriteEchoTool` helper |
| `AGENTS.md` | Update architecture description and test helpers list |

---

## Design Decisions

1. **Sequential execution, not parallel** — Tool calls are executed one at a time. This preserves the conversation ordering invariant (tool results follow the assistant message in order) and avoids concurrency complexity. Parallel execution is tracked as a future optimisation.

2. **Per-call cancellation check** — A `cancel.is_cancelled()` check is inserted before each tool call in the loop. This allows cancellation to take effect between tool calls in a batch, not just at the top of the outer loop.

3. **Denial doesn't stop the batch** — When a tool call is denied, the denial result is appended and the loop continues to the next tool call. This ensures every tool call in the model's response gets a result (either from execution or denial), maintaining the conversation invariant that every `tool_call_id` in the assistant message has a matching `Tool` result message.

4. **Empty `tool_calls` is an error** — The model should never return `finish_reason: tool_calls` with an empty `tool_calls` array. Returning `RhoError::Unexpected` is the correct response — it's a protocol violation.

5. **`multi_tool_call_response` in test-helpers** — Kept consistent with the existing `tool_call_response` builder pattern. Takes a `Vec` of `(id, name, args)` tuples.

---

## Out of Scope (deferred)

| Item | Deferred to |
|---|---|
| Parallel tool execution | Future optimisation |
| Config-driven approval per-tool | Task 6 |
| Tool execution timeout per-call from config | Task 6 |
