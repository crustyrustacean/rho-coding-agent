# Context Management

The context manager is responsible for fitting the conversation within the model's context window. It decides which messages to keep, which to evict, and how to render compacted entries — using graduated resolution, phase-aware summaries, and selective eviction to maximize useful context.

## The problem

Language models have finite context windows. A long coding session — especially one involving tool calls with large outputs — can easily exceed the window. When that happens, something has to give. The naive approach (drop the oldest message) silently discards the user's original request, causing the *amnesia bug* where the model forgets what it was asked to do.

## Token budget

`TokenBudget` controls how much of the context window the agent is allowed to use for the conversation history:

```rust
TokenBudget {
    context_window: 32768,      // total window size (token_budget)
    completion_reserve: 16384, // reserved for the model's reply
}
// prompt_budget() = context_window - completion_reserve = 16384
```

The `completion_reserve` ensures the model always has room to generate a reply. The `prompt_budget()` is the hard limit on how many tokens of conversation history are sent.

## Sliding window

`SlidingWindowContextManager` is the default implementation. It:

1. Takes the message chain from the session's current branch (`path_messages()`).
2. Estimates token counts using the configured `TokenEstimator`.
3. Evicts the oldest *middle* messages to fit within `prompt_budget()`.
4. **Pins** the system message, the first user turn, and the last turn — these are never evicted.

The pinning of the first user turn is the fix for the amnesia bug: no matter how long the conversation, the model always sees what the user originally asked.

## Graduated resolution

Entries can exist at multiple fidelity levels, not just binary Full/Compacted. The context manager renders each entry according to its resolution:

| Resolution | Rendering |
|---|---|
| `Full` | Verbatim message content |
| `Outlined` | Structural summary: tool name, key arguments, truncated output (~10-20% of original) |
| `Summarized` | Prose summary: one-line description of what the entry represents (~5-10% of original) |
| `Pinned` | Protected from eviction; rendered at its native resolution |
| `Compacted` | Replaced by a compaction summary (a synthetic user message) |
| `Attached` | Brief mention (e.g., "Tool call X was executed") |

This is the *adaptive mesh refinement* metaphor: full fidelity where the model needs it, coarsened where it doesn't. The system can downgrade entries through `Full → Outlined → Summarized` before resorting to eviction.

### Selective turn-internal eviction

Instead of evicting entire turns when the context window fills up, the eviction planner (`plan_downgrades`) analyzes the entry path and produces a plan that downgrades individual entries *within* turns. The `prepare_context()` method applies this plan before building messages, reducing the need for turn-level eviction.

Invariants: the system message, first user turn, last turn, and pinned entries are never downgraded. Tool-pair integrity (assistant + tool result) is maintained.

## Phase detection

The agent loop tracks which phase of work is currently active:

- **Exploration** — reading files, listing directories, gathering context
- **Execution** — writing files, editing code
- **Verification** — running `cargo check`, `cargo test`, `cargo clippy`
- **Conclusion** — final text reply from the model

Phase transitions follow a state machine: Exploration → Execution → Verification → Conclusion, with back-transitions allowed (e.g., Verification → Execution when a test fails).

## Compaction

When older entries are compacted (see [Sessions](./sessions.md)), the compaction system produces structured summaries.

### Phase-aware compaction

Compaction summaries are grouped by phase, producing a narrative that tells the model *what happened* in each phase:

```text
**Exploration:** Read main.rs, lib.rs (2 reads).
**Execution:** Edited src/parser.rs (3 edits).
**Verification:** cargo_check, cargo_test. Found: 1 error, 0 warnings.
```

### LLM compaction (optional)

When `compaction_mode = "llm"` is configured, the compaction system calls the LLM to generate a narrative summary in the `notes` field of `CompactionSummary`. This runs a two-stage process:

1. **Mechanical compaction** — structured fields (tool calls, findings, phases) are always computed deterministically.
2. **LLM narrative** — the model writes a concise paragraph summarizing what was accomplished, stored in `notes`.

On LLM failure, the system falls back to mechanical-only compaction — no error propagation to the agent loop.

### Auto-compact

When context utilization crosses the `auto_compact_threshold` (configurable), the agent loop proactively compacts older entries before eviction is needed. This prevents the abrupt context loss that occurs when turn-level eviction kicks in.

## Tool schema overhead

Tool schemas (the JSON descriptions sent to the model so it knows what tools are available) consume tokens. The context manager subtracts an estimate of this overhead from the available budget before fitting messages, preventing the combined total from exceeding the window.

## Token estimation

`TokenEstimator` is a trait for estimating token counts without calling the model's tokenizer. The default implementation (`HeuristicEstimator`) uses:

- **Per-model calibration**: an exponential moving average (α=0.3) that learns the real tokens-per-character ratio from model responses.
- **Bootstrap ratios**: built-in starting ratios for common model families (Gemma, Qwen, Claude, GPT-4, Llama).
- **Substring matching**: falls back to character-level estimation for non-ASCII text.

The estimator converges to within 10% accuracy by the third model call in most cases.

## Enhanced context stats

`ContextStats` provides rich token distribution diagnostics:

```rust
pub struct ContextStats {
    // Basic stats
    pub context_window: usize,
    pub completion_reserve: usize,
    pub estimated_used: usize,
    pub message_count: usize,
    pub entry_count: usize,
    pub path_entry_count: usize,

    // Token distribution by message role
    pub role_tokens: RoleTokenDistribution,      // system, user, assistant, tool

    // Token distribution by resolution level
    pub resolution_tokens: ResolutionTokenDistribution,  // full, outlined, summarized, pinned

    // Token distribution by session phase
    pub phase_tokens: PhaseTokenDistribution,    // exploration, execution, verification, conclusion, unclassified

    // Compaction tracking
    pub compaction_tokens: usize,
    pub compacted_entry_count: usize,
}
```

Key methods:
- `utilization_percent()` — context utilization as 0–100% (based on prompt budget)
- `estimated_remaining()` — remaining tokens in the prompt budget (saturates at 0)
- `prompt_budget()` — context window minus completion reserve

## Context status bar

A consumer (TUI, editor integration) can display a compact one-line status bar after every turn showing estimated token usage:

```text
[████████████░░░░░░░░] 12.3k/32k tokens (50%) │ 12.3k remaining │ 10 messages
```

The bar can be color-coded by utilization: green (<60%), yellow (60–80%), red (>80%). This gives a quick at-a-glance sense of how much context window remains.

For a detailed breakdown, the `getSessionStats` RPC method shows:

- Context window size and completion reserve
- Prompt budget (context window minus reserve)
- System prompt and tool schema overhead
- Conversation tokens
- Estimated used/remaining/percentage
- Message count, path entry count, total entries
- **Token distribution by role** (system, user, assistant, tool)
- **Token distribution by resolution** (full, outlined, summarized, pinned)
- **Compaction tokens and compacted entry count**