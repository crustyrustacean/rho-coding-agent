# Phase 3.8 Tasks

1. **Define streaming event types in `rho-core`:** ✅
   - Create `StreamEvent` enum with variants:
     - `Token { text: String }` — a delta text chunk from the model
     - `ReasoningStart` — the model began chain-of-thought (reasoning_content delta)
     - `ReasoningToken { text: String }` — a delta chunk within reasoning
     - `ToolCallStart { id: String, name: String }` — model started emitting a tool call
     - `ToolCallArgument { id: String, delta: String }` — incremental argument JSON
     - `ToolCallEnd { id: String }` — tool call arguments complete
     - `Finish { reason: FinishReason, usage: Option<ModelUsage> }` — stream complete
     - `Error { message: String }` — stream error
   - Ensure all variants are `Clone`, `Debug`, `Send`, `Sync` for channel transmission

2. **Add `ChatClient::chat_stream` method:** ✅
   - Extend `ChatClient` trait with:
     ```rust
     async fn chat_stream(
         &self,
         request: ChatRequest,
     ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;
     ```
   - Implement for `LocalChatClient`:
     - Set `stream: true` in the request body
     - Use `reqwest::Response::bytes_stream()` to read SSE events
     - Parse SSE lines (`data: {...}`) and deserialize to OpenAI streaming format
     - Accumulate delta content per field (content, reasoning_content, tool_calls)
     - Emit appropriate `StreamEvent` variants
   - Ensure SSE parsing handles:
     - Multi-line JSON payloads (some providers split deltas across events)
     - Empty data lines (`data: [DONE]`)
     - Connectivity errors (convert to `StreamError` event)

3. **Implement SSE parser for OpenAI streaming format:** ✅
   - Create `StreamParser` in `rho-core/src/stream.rs`
   - Parse SSE line format: `data: <json_payload>`
   - Handle `[DONE]` sentinel to signal end of stream
   - Accumulate partial JSON when deltas span multiple SSE events
   - Deserialize streaming response format (different from non-streaming):
     - `choices[0].delta.content` (partial text)
     - `choices[0].delta.reasoning_content` (reasoning delta)
     - `choices[0].delta.tool_calls[]` (incremental tool call objects)
   - Track tool call state by index and assign stable IDs for `ToolCallStart`/`ToolCallEnd`

4. **Add agent event types for UI updates:** ✅
   - Create `AgentEvent` enum in `rho-core/src/agent.rs`:
     - `StateChanged { from: AgentState, to: AgentState }`
     - `Token { text: String }` — model-generated text
     - `ReasoningToken { text: String }` — reasoning content
     - `ToolCallRequested { call: ModelToolCall, approved: Option<bool> }`
     - `ToolOutputChunk { tool_name: String, text: String }` — incremental tool output
     - `ToolOutputComplete { tool_name: String, result: ToolResult }`
     - `Error { message: String }`
   - All variants are `Clone`, `Debug`, `Send`, `Sync` for broadcast channels

5. **Wire broadcast channel into agent loop:** ✅
   - Add `event_tx: tokio::sync::broadcast::Sender<AgentEvent>` to `run_loop` signature
   - Create `broadcast::channel(16)` for up to 16 buffered events
   - Emit state changes: before `send_with_retry`, emit `StateChanged(Idle, Thinking)`
   - Emit token events: when consuming `StreamEvent::Token`, forward as `AgentEvent::Token`
   - Emit tool events: before/after tool execution, emit `ToolCallRequested` → `ToolOutputChunk` → `ToolOutputComplete`
   - On finish, emit `StateChanged(Thinking/ExecutingTool, Idle)`
   - Ensure all `#[allow(unused_assignments)]` are removed for `state` variable — now used for event emission

6. **Make agent loop streaming-aware:** ✅
   - Refactor `send_with_retry` to accept a flag `use_streaming: bool`
   - When streaming is enabled:
     - Call `client.chat_stream()` instead of `client.chat()`
     - Consume stream events as they arrive
     - Accumulate full content in a buffer for the final `AssistantResponse`
     - Emit `AgentEvent::Token` for each `StreamEvent::Token`
     - Handle `StreamEvent::Error` as a retryable error
   - When streaming is disabled (backward compatibility):
     - Use existing `client.chat()` (non-blocking)
     - Emit full text as a single `AgentEvent::Token` on completion
   - Ensure tool call detection works for both streaming and non-streaming paths

7. **Add streaming support to `ToolRegistry::execute`:** ✅
   - Extend `Tool::execute` to optionally return `ToolOutcome::Streamed(Receiver<ToolChunk>)`
   - Update `ToolRegistry::execute` to:
     - Accept `event_tx: broadcast::Sender<AgentEvent>` parameter
     - When outcome is `Streamed`:
       - Spawn a tokio task to consume chunks from the receiver
       - Emit `AgentEvent::ToolOutputChunk` for each chunk
       - Accumulate full output for final `ToolResult`
       - Emit `AgentEvent::ToolOutputComplete` when stream ends
     - When outcome is `Immediate`, emit a single `AgentEvent::Token` with full output
   - Ensure cancellation (`cancel` token) terminates streaming tasks cleanly

8. **Implement streaming `RunCommand` tool:** ✅
   - Modify `PowerShellExecutor` to support incremental output capture
   - Use `tokio::process::Command` with `stdout(Stdio::piped())` and `stderr(Stdio::piped())`
   - Create a tokio channel (`mpsc::unbounded_channel()`) for line-by-line output
   - Spawn tasks to read stdout/stderr and send chunks via the channel
   - Return `ToolOutcome::Streamed(receiver)` instead of buffering all output
   - Ensure exit code is captured and reported in the final `ToolResult`

9. **Add optional streaming flag to `AgentConfig`:** ✅
   - Add `streaming: bool` field (default: `false` for backward compatibility)
   - Pass through from `RhoConfig.agent.streaming` or CLI flag `--streaming`
   - Control whether `send_with_retry` uses streaming or blocking API
   - Document that streaming is preferred for interactive sessions, but eval uses non-streaming

10. **Refactor `run_loop` for state-change emission:** ✅
    - Remove `#[allow(unused_assignments)]` — `state` variable is now actively used
    - Emit `StateChanged` before each state transition:
      - On entry: `StateChanged(Idle, Thinking)`
      - Before tool approval: `StateChanged(Thinking, AwaitingApproval)`
      - After approval: `StateChanged(AwaitingApproval, ExecutingTool)` (or `Thinking` if denied)
      - After tool execution: `StateChanged(ExecutingTool, Thinking)`
      - On completion: `StateChanged(Thinking, Idle)`
    - Ensure all code paths through the loop emit a state change

11. **Add `Session::append_assistant_message_streaming` helper:** ✅
    - Create a method to accumulate streaming deltas into the session tree
    - Accept streaming events (token deltas, tool calls) and build the final message
    - Persist the complete assistant message when the stream finishes
    - Handle the case where tool calls arrive incrementally (accumulate arguments)
    - Ensure the persisted message is identical to non-streaming format for compatibility

12. **Update `Session::send_current` to support streaming client:** ✅
    - Add a `streaming` parameter to control whether `chat_stream` or `chat` is called
    - When streaming: consume events, update session incrementally, return accumulated response
    - When not streaming: use existing logic (single request/response)
    - Return the same `AssistantResponse` type for both paths

13. **Add unit tests for streaming components:** ✅
    - `StreamEvent` serialization/deserialization tests
    - SSE parser tests:
      - Valid SSE line parsing
      - Multi-line JSON accumulation
      - `[DONE]` sentinel handling
      - Malformed line recovery
    - `ChatClient::chat_stream` mock tests using a test HTTP server
    - `ToolRegistry::execute` streaming path tests
    - `AgentEvent` broadcast channel tests:
      - Multiple subscribers receive all events
      - Lagging subscriber misses no events (buffer handling)
      - Channel closure propagation

14. **Add integration tests for streaming agent loop:** ✅
    - Create a `MockStreamingChatClient` in `rho-test-helpers`
    - Implement a mock that emits a predetermined sequence of `StreamEvent`s
    - Test agent loop with:
      - Simple text-only streaming response
      - Streaming response with reasoning content
      - Streaming response with tool calls
      - Stream error recovery (retry logic)
    - Verify that `AgentEvent`s are emitted in correct order
    - Verify that session persistence works with streaming (final message is stored)

15. **Add streaming command-line flag:** ✅
    - Add `--streaming` / `--no-streaming` flag to `rho` CLI
    - Override `RhoConfig.agent.streaming` when present
    - Pass through to `AgentConfig`
    - Default to non-streaming for backward compatibility (eval, tests)

16. **Documentation updates:** ✅
    - Update `README.md` to mention streaming capability
    - Document the `--streaming` flag in CLI help
    - Add inline documentation for `ChatClient::chat_stream`
    - Document `AgentEvent` variants and their consumer contract
    - Document the `ToolOutcome::Streamed` variant for tool authors
    - Update `AGENTS.md` with streaming architecture notes
    - Document the broadcast channel capacity and overflow behavior

17. **Backward compatibility verification:** ✅
    - Ensure existing tests pass without modification (all use non-streaming by default)
    - Verify that eval harness (`rho-eval`) works with non-streaming mode
    - Ensure that tools returning `Immediate` continue to work unchanged
    - Test that `--no-streaming` restores pre-Phase 3.8 behavior exactly
    - Verify that persisted sessions are compatible (no format changes from streaming)

18. **Performance testing:** ✅
    - Benchmark streaming vs non-streaming for large responses
    - Measure memory usage with streaming (should be lower — no buffering full response)
    - Test with real model servers (LM Studio, Ollama) to verify SSE parsing
    - Verify that 16-event broadcast buffer is sufficient (no dropped events under normal load)

19. **Error handling audit:** ✅
    - Ensure all streaming errors are retryable via `TransitionError::Retryable`
    - Verify that partial stream results are not persisted (only complete messages go into session)
    - Test connection drops mid-stream (should trigger retry)
    - Verify that tool stream errors propagate to `AgentEvent::Error`

20. **Final integration test:** ✅
    - Create an end-to-end test using streaming mode with a real mock client
    - Execute a multi-turn conversation with:
      - Text-only response (streaming)
      - Tool call execution (streaming tool output)
      - Error and retry (stream error → retry → success)
    - Verify that `AgentEvent`s are emitted in the correct order
    - Verify that the final session state matches expected output
