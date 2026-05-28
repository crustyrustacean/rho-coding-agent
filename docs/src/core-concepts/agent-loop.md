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
    observer: &dyn AgentObserver,
) -> Result<String>
```

`run_loop` takes a `Session` (not a `Conversation` — the session type was introduced in Phase 2.5). It:

1. Appends `message` as a user turn via `session.append_user_message()`.
2. Enters the Thinking state and sends the session to the model via `session.send_current(client)`.
3. Processes the model's response:
   - **Text reply** → return it (Idle).
   - **Tool calls** → for each call, check approval, execute, append the result, then loop back to Thinking.

All tool calls in a single model response are executed **sequentially**. Each result is appended before the session is re-sent to the model. Parallel execution is a future optimisation.

## AgentObserver

The `AgentObserver` trait receives live events from the agent loop so that a REPL, TUI, or test harness can render progress as it happens.

```rust
pub trait AgentObserver: Send + Sync {
    fn on_state_change(&self, _state: AgentState) {}
    fn on_text_delta(&self, _delta: &str) {}
    fn on_reasoning_delta(&self, _delta: &str) {}
    fn on_tool_call(&self, _name: &str, _arguments: &str) {}
    fn on_tool_result(&self, _name: &str, _result: &ToolResult) {}
    fn on_tool_denied(&self, _name: &str) {}
    fn on_approval_requested(&self, _tool_name: &str, _risk: ToolRisk) {}

    /// Called before a tool executes. Return Block to prevent execution.
    /// Used by extensions to implement approval gates and safety filters.
    fn on_tool_call_intercept(&self, _name: &str, _args: &str) -> Option<InterceptResult> {
        None
    }
}
```

All methods have default no-op implementations, so observers only need to override the events they care about. The REPL's `ReplObserver` streams reasoning deltas and tool activity to stdout so the user can see what the model is doing in real time. For tests, benchmarks, and headless use, `NopObserver` discards all events.

When extensions are loaded, the `CompositeObserver` fans out every call to both the REPL/RPC observer and the extension `DenoObserver`s. For `on_tool_call_intercept`, the **first `Block` wins** — if any extension blocks a tool call, execution is denied immediately.

The observer is called:
- At every state transition (`on_state_change`)
- As streaming deltas arrive (`on_text_delta`, `on_reasoning_delta`)
- Before and after each tool call (`on_tool_call`, `on_tool_result`, `on_tool_denied`)
- When approval is requested (`on_approval_requested`)

## Retry with backoff

Transient HTTP errors (503, 429, connection refused) are retried with exponential backoff. The retry budget is configurable via `AgentConfig`. If the budget is exhausted, `run_loop` returns `RhoError::RetryBudgetExhausted`.

## Cancellation

A `CancellationToken` is checked at the top of each loop iteration and between tool calls in a batch. When cancelled, `run_loop` returns `RhoError::Cancelled` immediately — no pending tool execution is aborted mid-flight, but no new ones are started.

## Iteration guard

The loop terminates after `config.max_iterations` tool-call rounds (default: 32). This prevents infinite loops from misbehaving models. Returns `RhoError::MaxIterationsExceeded`.

## Session persistence

Since `run_loop` operates on a `Session`, all appends (user messages, tool results) are automatically flushed to the JSONL session file if persistence is enabled. If the process crashes mid-loop, the session file contains everything up to the last successful append — at most one entry is lost.
