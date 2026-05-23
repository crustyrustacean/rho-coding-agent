# Context Management

The context manager is responsible for fitting the conversation within the model's context window. It decides which messages to keep, which to evict, and how to render compacted entries.

## The problem

Language models have finite context windows. A long coding session — especially one involving tool calls with large outputs — can easily exceed the window. When that happens, something has to give. The naive approach (drop the oldest message) silently discards the user's original request, causing the *amnesia bug* where the model forgets what it was asked to do.

## Token budget

`TokenBudget` controls how much of the context window the agent is allowed to use for the conversation history:

```rust
TokenBudget {
    context_window: 32768,     // total window size
    completion_reserve: 4096,  // reserved for the model's reply
}
// prompt_budget() = context_window - completion_reserve = 28672
```

The `completion_reserve` ensures the model always has room to generate a reply. The `prompt_budget()` is the hard limit on how many tokens of conversation history are sent.

## Sliding window

`SlidingWindowContextManager` is the default implementation. It:

1. Takes the message chain from the session's current branch (`path_messages()`).
2. Estimates token counts using the configured `TokenEstimator`.
3. Evicts the oldest *middle* messages to fit within `prompt_budget()`.
4. **Pins** the system message, the first user turn, and the last turn — these are never evicted.

The pinning of the first user turn is the fix for the amnesia bug: no matter how long the conversation, the model always sees what the user originally asked.

## Adaptive resolution

When the session has compacted entries (see [Sessions](./sessions.md)), the context manager uses the resolution level to decide how to render each entry:

| Resolution | Rendering |
|---|---|
| `Full` | Verbatim message content |
| `Compacted` | Compaction summary (a synthetic user message) |
| `Attached` | Brief mention (e.g., "Tool call X was executed") |

This is the *adaptive mesh refinement* metaphor: full fidelity where the model needs it, coarsened where it doesn't.

## Tool schema overhead

Tool schemas (the JSON descriptions sent to the model so it knows what tools are available) consume tokens. The context manager subtracts an estimate of this overhead from the available budget before fitting messages, preventing the combined total from exceeding the window.

## Token estimation

`TokenEstimator` is a trait for estimating token counts without calling the model's tokenizer. The default implementation (`HeuristicEstimator`) uses:

- **Per-model calibration**: an exponential moving average (α=0.3) that learns the real tokens-per-character ratio from model responses.
- **Bootstrap ratios**: built-in starting ratios for common model families (Gemma, Qwen, Claude, GPT-4, Llama).
- **Substring matching**: falls back to character-level estimation for non-ASCII text.
The estimator converges to within 10% accuracy by the third model call in most cases.

## Context status bar

The REPL displays a compact one-line status bar after every turn showing estimated token usage:

```text
[████████████░░░░░░░░] 12.3k/32k tokens (50%) │ 12.3k remaining │ 10 messages
```

The bar is color-coded by utilization: green (<60%), yellow (60–80%), red (>80%). This gives a quick at-a-glance sense of how much context window remains.

For a detailed breakdown, the `/status` REPL command (aliased as `/context`) shows:

- Context window size and completion reserve
- Prompt budget (context window minus reserve)
- System prompt and tool schema overhead
- Conversation tokens
- Estimated used/remaining/percentage
- Message count, path entry count, total entries