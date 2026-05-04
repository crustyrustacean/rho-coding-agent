# Agent Loop

The agent loop is the state machine at the heart of rho. It drives the conversation forward by sending messages to the model, executing tool calls, and feeding results back.

## State machine

```text
  ┌──────────────────────────────────────────────┐
  │                                              │
  ▼                                              │
Thinking ──► AwaitingApproval ──► ExecutingTool ──┘
  │
  ▼
Idle (done)
```

- **Thinking**: send the session to the model and wait for a response.
- **AwaitingApproval**: if a tool call requires human confirmation, ask the user. On denial, feed a synthetic "denied" result and return to Thinking.
- **ExecutingTool**: run the tool and append the result to the session.
- **Idle**: the model returned a text reply — the loop is done.

## The `run_loop` function

```rust
pub async fn run_loop(
    session: &mut Session,
    message: &str,
    client: &dyn ChatClient,
    registry: &ToolRegistry,
    config: &AgentConfig,
    cancel: CancellationToken,
    gate: &dyn ApprovalGate,
) -> Result<String>
```

`run_loop` takes a `Session` (not a `Conversation` — the session type was introduced in Phase 2.5). It:

1. Appends `message` as a user turn via `session.append_user_message()`.
2. Enters the Thinking state and sends the session to the model via `session.send_current(client)`.
3. Processes the model's response:
   - **Text reply** → return it (Idle).
   - **Tool calls** → for each call, check approval, execute, append the result, then loop back to Thinking.

All tool calls in a single model response are executed **sequentially**. Each result is appended before the session is re-sent to the model. Parallel execution is a future optimisation.

## Retry with backoff

Transient HTTP errors (503, 429, connection refused) are retried with exponential backoff. The retry budget is configurable via `AgentConfig`. If the budget is exhausted, `run_loop` returns `RhoError::RetryBudgetExhausted`.

## Cancellation

A `CancellationToken` is checked at the top of each loop iteration and between tool calls in a batch. When cancelled, `run_loop` returns `RhoError::Cancelled` immediately — no pending tool execution is aborted mid-flight, but no new ones are started.

## Iteration guard

The loop terminates after `config.max_iterations` tool-call rounds (default: 100). This prevents infinite loops from misbehaving models. Returns `RhoError::MaxIterationsExceeded`.

## Session persistence

Since `run_loop` operates on a `Session`, all appends (user messages, tool results) are automatically flushed to the JSONL session file if persistence is enabled. If the process crashes mid-loop, the session file contains everything up to the last successful append — at most one entry is lost.
