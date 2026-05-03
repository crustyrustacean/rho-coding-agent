# Phase 2.5 Context-Management Follow-ups

**Companion to:** `rho-coding-agent-phase2-review.md`
**Plan it folds into:** `phase-2.5/phase.md` and `phase-2.5/tasks.md`
**Date:** 2026-05-01 (originally), updated 2026-05-02
**Codebase version:** 0.11.0 (Phase 2 complete)

---

## Why this is its own document

These items were originally a fourth section of the Phase 2 review, alongside High / Medium / Low. They were lifted out for two reasons:

1. **They share a root cause.** All ten describe symptoms of the linear `Vec<ChatMessage>` conversation model under context pressure — the heuristic is wrong, the budget doesn't account for tool schemas, eviction is destructive, and there is no test exercising real pressure. Treating them as ten independent fixes is the wrong shape.

2. **The Phase 2.5 session-tree refactor changes which of them are still bugs.** Several get absorbed by the new entry/leaf data model for free; others survive but become natural sub-tasks within the refactor; a couple are independent and remain.

The new Phase 2.5 (adaptive-resolution context) is *not* the same thing as this list. Phase 2.5 is a structural refactor framed around adaptive mesh refinement (see `phase.md`). This document is its punch list of context-management correctness items — most of which the refactor either dissolves or makes easier to fix.

The naming collision is unfortunate but accurate: the Phase 2 review identified ten "Phase 2.5" items because the issues warranted their own phase, and the refactor the review prompted now occupies that slot.

---

## Status legend

| Status | Meaning |
|---|---|
| **Absorbed** | The Phase 2.5 refactor makes this stop being a bug. No action needed. |
| **Folded** | Survives the refactor but becomes a sub-task within a specific Phase 2.5 task. |
| **Independent** | Unaffected by the refactor; needs its own fix. |

---

## P2.5-1. The token estimator is model-blind and can be wrong by 100%+

**Status:** Folded into Phase 2.5 Task 6 (calibrator) — **early-priority within the phase**.

```rust
let tokens = chars.div_ceil(4);
```

The `4 chars/token` heuristic was originally identified as "decent for prose English but wrong by 20-30% on dense code." Today's empirical results (2026-05-02) show the situation is much worse: the error is *model-dependent*, and on Qwen 2.5 Coder it can be 100%+. The `chars/4` heuristic estimates roughly half the actual tokens Qwen consumes per message. There is no single fixed number that's safe across models.

The bias is also *systematic in the wrong direction*: it underestimates most heavily on exactly the dense-ASCII output that triggers P2.5-9 (PowerShell tool results, `cargo test` verbose output, large config dumps). The amnesia bug compounds: the heuristic says "you have headroom," the budget says "fit comfortably," and the model's actual context window blows past while `fit` thinks it has plenty of room.

**The fix in Phase 2.5 is a self-correcting calibrator** rather than a better fixed heuristic. The `TokenEstimator` trait records actual `prompt_tokens` from each API response and updates a per-model ratio via exponential moving average. Bootstrap values are model-specific (Gemma 3.5, Qwen 2.5, Claude 3.7, GPT-4 4.0); unknown models bootstrap conservatively at 2.5 and converge from there.

This is the cleaner answer than committing to `tiktoken-rs` outright — it works for local models that don't have a tiktoken vocabulary, and it adapts to model variants without per-version tuning. The `tiktoken` integration is feature-gated as a parallel option for users who want bit-exact estimation on OpenAI/Claude-format models.

**See also:** `Amnesia_Test_-_Qwen2_5-Coder-14B.md` for the empirical evidence, particularly the iteration-0 comparison showing Qwen reporting 5,956 prompt_tokens for a conversation Gemma reported as ~2,800.

---

## P2.5-2. `ContextManager::fit` doesn't account for the tools schema

**Status:** Folded into Phase 2.5 Task 8 (context building).

`Conversation::send_current` sends `messages` (which goes through `fit`) **plus** `tools: self.tools.clone()`. The token budget is enforced only on messages. With 5+ tools whose JSON schemas can be 200–500 tokens each, you are sending 1–3K tokens of fixed overhead **outside the budget**.

**Fix:** `fit_path` (introduced in Task 8) computes tool-schema tokens using the calibrated estimator and subtracts them from the prompt budget *before* fitting messages. Same place that subtracts system-message overhead (P2.5-3).

---

## P2.5-3. System overhead grows monotonically without warning

**Status:** Folded into Phase 2.5 Task 14 (binary update).

The system message is pinned at the root and never evicted. As tools, context files, and prompt extensions accumulate, system overhead eats into the conversation budget silently. The 32K default raised in Phase 2 Task 13 was a fix for the symptom; the underlying issue — no visibility into how much budget is actually available for conversation — is unchanged.

The new design does not make this worse, but the leaf-to-root path computation depends on knowing what fraction of the budget is even available.

**Fix:** at startup, after constructing the `Session`, compute the system message + tool schemas + context files token cost using the calibrated estimator. If it exceeds 50% of the budget, log a `warn!` with a breakdown:

```
warn: system overhead is 12,400 / 32,768 tokens (38%)
   - base prompt:    2,200
   - AGENTS.md:      4,500
   - tool schemas:   3,100
   - PowerShell ext: 1,500
   - Rust ext:       1,100
```

This is roughly what Phase 5 plans as "budget-aware composition" but the 30-line warning version pays off now.

---

## P2.5-4. `Conversation::clear` retains all `System` messages

**Status:** **Absorbed.**

```rust
self.messages.retain(|m| matches!(m, ChatMessage::System { .. }));
```

In the new design "clear" becomes "branch back to the system message," which by construction has exactly one root entry. The defensive concern goes away because the data model enforces it: only one entry has `parent_id = None`.

A `debug_assert!` on session construction that the root payload is `Message(System)` is still worth adding, but the bug as originally stated cannot occur.

---

## P2.5-5. Turn boundary doesn't handle interleaved system messages

**Status:** **Absorbed.**

The old `SlidingWindowContextManager::group` could be confused by mid-conversation system messages, both losing them and overwriting earlier ones in its tracking variable. In the new design there is one root entry, full stop. If extensions want to inject system-style guidance, they use `CustomMessage` with their own framing.

The grouping logic in `fit_path` simplifies considerably because it does not need to handle this edge case.

---

## P2.5-6. No notion of priority within a turn

**Status:** Folded into Phase 2.5 Task 9 (compaction strategy) — **made type-safe rather than convention-based**.

When the budget forces eviction, the oldest turn is dropped. For a long, multi-tool conversation, that is typically the user's *original request* — exactly the message you want to preserve.

The original plan addressed this by having `MechanicalCompactionStrategy` always include the original user request in its summary text, and writing a test verifying the property held. The revised plan goes further: the `CompactionSummary` type has a dedicated `original_request: Option<String>` field, and the rendering contract guarantees it appears in the synthetic message. The property is enforced by the type signature, not by convention.

```rust
pub struct CompactionSummary {
    pub original_request: Option<String>,  // ← dedicated field
    pub tool_calls: BTreeMap<ToolName, Vec<String>>,
    pub tokens_compacted: usize,
    pub entry_count: usize,
    pub time_span: Duration,
    pub notes: Option<String>,
}
```

A consumer that forgets to render `original_request` is now a code review item, not a silent data loss.

---

## P2.5-7. No integration test exercising actual budget pressure

**Status:** **Already covered.** This is Task 17 of the new Phase 2.5.

The new test plan is more demanding than the original review item asked for: three scenarios must pass, including the captured `amnesia_test_small.md` reproducer (binary check: does the secret survive?) and the Get-Process oversized-tool-result regression test from `Context_Window_Amnesia_Bug.md`. Phase 2.5 Task 17 specifies all three.

---

## P2.5-8. No way to signal "I had to drop the user's request"

**Status:** Partially folded into Phase 2.5 Task 9.

In the new design, eviction-via-compaction *is* visible — there is a `Compaction` entry on the path, and the `CompactionSummary` it carries surfaces `original_request`, `tool_calls`, `tokens_compacted`, and `entry_count`. The TUI in Phase 4 can render this directly from the structured data, no parsing required.

The original write-up suggested a separate `CompactionDetails` struct alongside `Compaction`. The revised plan collapses this: the `CompactionSummary` *is* that struct. One less concept; same information surfaced.

---

## P2.5-9. Token budget overflow during a single tool result

**Status:** Folded into Phase 2.5 Task 5 (append operations) — **early-priority within the phase**.

This is **not hypothetical**. A documented session in `Context_Window_Amnesia_Bug.md` shows the bug firing on `Get-Process | Format-List Name, Id, CPU, VM, WorkingSet` — a single PowerShell command whose output (50K–200K characters) exceeds the entire 32K token budget on its own. The current `SlidingWindowContextManager` responds by evicting the very tool result that just came back, along with the user message that prompted it, leaving the model with `[system, <nothing>]` and producing the generic "how can I help?" response that gave the bug its name.

A *second* reproducer (`test_scenarios/amnesia_test_small.md`) shows the same pathology firing under sustained pressure rather than a single oversized result: 6 file reads × ~5K tokens each push the conversation past budget, and the user turn carrying the test secret gets evicted. This reproducer fires on both Gemma 4 and Qwen 2.5 Coder 14B, with different shapes (Gemma evicts a complete turn after 5 reads; Qwen evicts just the user message after 3 reads), but identical end result: the model loses the original instructions and either gives a wrong answer or an empty one.

The data model refactor does not solve this on its own. Whether you store tool output as a `ChatMessage::Tool` in a `Vec` or as a `Message` payload in an `Entry`, an oversized result still blows the budget if it enters history un-truncated.

**Fix in Task 5:** `Session::append_tool_result` truncates the redacted content if it exceeds the per-result threshold (default: half the prompt budget, configurable). The truncated message includes a footer noting the original size. The full content is preserved out-of-band as `ToolResultDetails::FullOutput { original_size, content }` on the `ToolResult` — preserving information at higher resolution where it doesn't consume context budget. This is the AMR move at the leaf level: coarse content on the model's grid, fine content attached for retrieval.

```rust
const MAX_TOOL_RESULT_FRACTION: f32 = 0.5;
let max_tokens = (budget.prompt_budget() as f32 * MAX_TOOL_RESULT_FRACTION) as usize;
if estimator.estimate(&redacted) > max_tokens {
    let max_chars = chars_to_fit_tokens(&redacted, max_tokens, estimator);
    let original_size = redacted.len();
    let truncated = format!(
        "{}\n\n... [truncated; original size: {original_size} bytes — re-read the source with offset to access more].",
        &redacted[..floor_char_boundary(&redacted, max_chars)],
    );
    // store truncated in entry payload, original in ToolResultDetails::FullOutput
}
```

(`floor_char_boundary` here is the helper from H6 — both fixes converge.)

**See also:**
- `Context_Window_Amnesia_Bug.md` for the original Get-Process trace.
- `Amnesia_Test_-_Context_Window_Eviction.md` for the small-test Gemma reproducer.
- `Amnesia_Test_-_Qwen2_5-Coder-14B.md` for the same test on Qwen.

---

## P2.5-10. `TokenBudget` is a single number, not structured

**Status:** Folded into Phase 2.5 Task 6 (calibrator) and Task 8 (context building).

Real model APIs have *prompt* token limits and separate *completion* token limits. `TokenBudget` conflates the two — it is used as the prompt limit, but the request also needs to leave room for the model's reply. With `max_tokens` set to context window (e.g., 32K) and prompt at 31K, the model may fail to produce a useful reply.

The new `TokenBudget` separates the two:

```rust
pub struct TokenBudget {
    pub context_window: usize,
    pub completion_reserve: usize,  // default 4096
}
impl TokenBudget {
    pub fn prompt_budget(&self) -> usize {
        self.context_window.saturating_sub(self.completion_reserve)
    }
}
```

A reasonable default for `completion_reserve` is 4,096 — enough for a substantial reply without hard-coding an assumption that would break for a 1M-context model. `fit_path` calls `prompt_budget()` everywhere it currently uses `max_tokens`.

---

## Summary

| ID | Status | Phase 2.5 task | Notes |
|---|---|---|---|
| P2.5-1 | Folded | Task 6 | **Early-priority.** Empirically wrong by 100%+ on Qwen; replaced with self-correcting calibrator |
| P2.5-2 | Folded | Task 8 | |
| P2.5-3 | Folded | Task 14 | |
| P2.5-4 | Absorbed | — | data model precludes it |
| P2.5-5 | Absorbed | — | data model precludes it |
| P2.5-6 | Folded | Task 9 | Made type-safe via `CompactionSummary::original_request` |
| P2.5-7 | Already covered | Task 17 | Now covers two captured reproducers + the Get-Process regression |
| P2.5-8 | Folded | Task 9 | Subsumed into `CompactionSummary` |
| P2.5-9 | Folded | Task 5 | **Early-priority.** Two reproducers in `test_scenarios/`; fires on both Gemma and Qwen |
| P2.5-10 | Folded | Tasks 6, 8 | Structured `TokenBudget` + calibrator together |

Two items absorbed by the refactor for free. Eight items that survive but slot into specific tasks of the Phase 2.5 plan. Zero items that remain independent.

**Order of attack within Phase 2.5:** Tasks 5 (bounded tool results) and 6 (calibrator) are the two early-priority items. Together they close the shipping amnesia bug currently exposed by both reproducers in 0.11.0. They could plausibly ship as a 0.11.x patch release before the rest of Phase 2.5 lands — see the "Optional pre-Phase-2.5 patch release" note at the end of `tasks.md`.

The consolidated pre-Phase-3 punchlist:

1. **High-priority security fixes** (H1–H6 from the review) — independent, do whenever.
2. **Phase 2.5 adaptive-resolution refactor** (`phase-2.5/phase.md`) — addresses every context-management concern in this document. Within it, Tasks 5 and 6 are the early-priority pair that close the amnesia bug; the rest is the structural work that prevents future variants of the same bug.
3. **M and L items from the review** — independent, do opportunistically.
