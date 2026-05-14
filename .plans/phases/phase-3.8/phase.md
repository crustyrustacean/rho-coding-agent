# Phase 3.8: Streaming Support

**Goal:** Enable streaming responses from the model API and incremental tool output rendering.

**Milestone:** The agent receives model responses token-by-token via SSE/streaming API, rendering tokens as they arrive. Tool output can also be streamed incrementally through Tokio channels, providing real-time feedback during long-running operations.

**Current state (pre-Phase 3.8):** The agent uses a non-streaming `ChatClient::chat` method that waits for the complete response. Tool execution returns `ToolOutcome::Immediate(ToolResult)` — the entire output is buffered before being sent to the model. `AgentState` exposes observable states for UI rendering, but state changes are not emitted via channels. Tokio is available as a dependency for async runtime.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `tokio` **(foundation)** | All streaming | Already in workspace — provides async runtime, channels (`tokio::sync::mpsc`, `tokio::sync::broadcast`), and SSE/streaming utilities |
| `reqwest` **(streaming)** | `rho-core` | Already in workspace for HTTP client — has native SSE/streaming support via `reqwest::Response::bytes_stream()` |

No new crates required — Tokio and reqwest are already available and support streaming.

## Decisions

**Streaming model responses:** Use OpenAI's SSE streaming API (`stream: true` parameter). The `ChatClient` trait gains a new `chat_stream` method that returns a stream of `StreamEvent` tokens. Non-streaming `chat` remains available for backward compatibility (e.g., eval harness doesn't need streaming).

**Streaming tool output:** Leverage existing `ToolOutcome::Streamed(Receiver<ToolChunk>)` variant. Tools that can produce incremental output (e.g., `RunCommand` with long-running processes) return a `tokio::sync::mpsc::Receiver`. The agent loop consumes chunks and emits them via a state-change channel for the TUI.

**State change channel:** Introduce an `AgentEvent` type and `tokio::sync::broadcast` channel for real-time UI updates. The agent loop emits events (state transitions, tokens, tool chunks) that the TUI subscribes to. This decouples the agent loop from the UI — multiple consumers can listen (TUI, logging, tests).

**Token ordering:** SSE events from OpenAI are guaranteed to arrive in order. We concatenate delta chunks per turn and emit `Token` events as they arrive. The final complete text is stored when `finish_reason` is received.

**Error handling in streams:** A `StreamError` event is emitted if the stream terminates unexpectedly. The agent loop converts streaming errors to the existing retry logic — if the stream fails mid-response, it may be retried depending on `RhoError::is_retryable()`.

## Exit Criteria

The agent supports streaming model responses and tool output. `ChatClient` has both `chat` (blocking) and `chat_stream` (async streaming) methods. The agent loop emits real-time events via a broadcast channel. Tools can optionally return streaming output. The TUI (Phase 4) can subscribe to events and render incrementally. The existing non-streaming behavior is preserved for eval and backward compatibility.
