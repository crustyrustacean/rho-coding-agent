# Phase 1a Tasks

1. **Refine the data model in `rho-core`:**
   - Audit existing types for ergonomics — can they be constructed without boilerplate? Are they composable?
   - Introduce newtypes where they prevent misuse: `FilePath` (declared here as a newtype around `PathBuf`; sandbox enforcement is added in Phase 1b), `ToolName`, `ToolCallId`, `DiagnosticCode`
   - Decide newtype `Deref` policy explicitly: `FilePath` implements `Deref<Target = Path>`, `ToolName` and `ToolCallId` implement `Deref<Target = str>`. This avoids constant `.0` access at API boundaries while preserving type safety internally. Document the decision in the crate-level doc comment.
   - Ensure every type round-trips through serde (JSON for API, TOML for future config)
   - Add builder-style or `From` impls for common construction patterns
   - Document the design philosophy in the crate-level doc comment

2. **Reshape `ChatMessage` to admit content blocks and tool-result variants:**
   The current `ChatMessage { role: Role, content: String }` will not survive contact with the real API surface. The OpenAI Chat Completions API accepts `content` as either a string or an array of typed parts (`{type: "text", ...}`, `{type: "image_url", ...}`, `{type: "file", ...}`); Anthropic requires the array form for anything beyond plain text; tool-result messages need a `tool_call_id` field that text messages don't have; assistant messages with `tool_calls` need a slot for the list of calls. Encoding all of this as `String` content forces every downstream type, fixture, and tool to carry assumptions that have to be unwound later.

   Define `ChatMessage` as a variant-based shape from the start. One viable form:
   ```rust
   pub enum ChatMessage {
       System  { content: Vec<ContentBlock> },
       User    { content: Vec<ContentBlock> },
       Assistant { content: Vec<ContentBlock>, tool_calls: Vec<ModelToolCall> },
       Tool    { tool_call_id: ToolCallId, content: Vec<ContentBlock> },
   }
   pub enum ContentBlock {
       Text { text: String },
       // Image / File / etc. added as needed — do not need impls in Phase 1a, but the variant point exists
   }
   ```
   The `Role` enum stays as a serialization-time discriminator (the wire format still uses `"role": "user"` etc.) but is no longer the carrier of message-shape information at the type level. Provide convenience constructors so common cases stay one-liners (`ChatMessage::user_text("hello")`, `ChatMessage::assistant_text("hi")`).

   Serialization must produce the existing OpenAI wire format: a single text block serialises as `"content": "..."` (string), multiple blocks or non-text blocks serialise as the array form. Add round-trip tests for both shapes.

3. **Define the `Tool` trait in `rho-core`:**
   ```rust
   #[async_trait]
   pub trait Tool: Send + Sync {
       fn name(&self) -> ToolName;
       fn description(&self) -> &str;
       fn parameters_schema(&self) -> serde_json::Value;
       fn risk(&self) -> ToolRisk;

       async fn execute(
           &self,
           arguments: serde_json::Value,
           cancel: CancellationToken,
       ) -> Result<ToolOutcome>;
   }
   ```

   The signature is locked down now because retrofitting it later means touching every `Tool` impl in the workspace. Three things the signature commits to:
   - **Async** — tools like `RunCommand` and `CargoTest` run for seconds or minutes; a synchronous `execute` would deadlock the runtime.
   - **Cancellable** — long cargo builds need to respond to Ctrl-C. `CancellationToken` (from `tokio_util::sync::CancellationToken` or our own thin wrapper) is passed through. Tools are expected to check it at I/O boundaries; in Phase 1a the three stub tools may simply abort at obvious points, but the parameter is wired through end-to-end.
   - **Streaming-capable result** — `ToolOutcome` is an enum that admits an incremental form (`ToolOutcome::Streamed(impl Stream<Item = ToolChunk>)`) alongside the immediate form (`ToolOutcome::Immediate(ToolResult)`). Phase 1a only emits `Immediate`; the bare REPL only consumes `Immediate`. The variant point exists so Phase 4's streaming TUI is a new variant, not a workspace-wide signature change.

   `#[async_trait]` is required because `ToolRegistry` stores `Box<dyn Tool>` and native AFIT is not dyn-compatible. See Phase 1a `phase.md` for the rationale.

4. **Define `ToolResult`, `ToolOutcome`, `ToolRisk`, and `ToolRegistry` in `rho-core`.** Tools are registered with a `ToolRisk` (`Read`, `Write`, `Destructive`) — the value lives on the type now even though Phase 1a does not consult it; Phase 1b's `ApprovalPolicy` is the first consumer.

5. **Define the `ChatClient` provider trait in `rho-core`:**
   ```rust
   #[async_trait]
   pub trait ChatClient: Send + Sync {
       async fn chat(&self, request: ChatRequest) -> Result<ModelResponse>;
   }
   ```
   Same dyn-compatibility reasoning as `Tool`: the binary swaps providers at runtime via `/provider`, which means `Box<dyn ChatClient>`, which means `#[async_trait]`. The trait is minimal — one method, provider-agnostic. Mock implementations return canned responses without a live server. `LocalChatClient` is the default concrete type (talks to `localhost` OpenAI-compatible endpoints like LM Studio or Ollama). Streaming (`chat_stream`) is added in Phase 4 as a new method with a default impl that wraps `chat`; non-streaming providers don't need to change.

6. **Implement the agent loop in `rho-core` as a state machine:**
   - Define `AgentState` as the four real states: `Idle`, `Thinking`, `AwaitingApproval`, `ExecutingTool`. (Approval handling lands in Phase 1b; the `AwaitingApproval` state is declared in Phase 1a but the loop doesn't enter it until the policy is wired up.)
   - Errors and retries are *transition outcomes*, not states. Model the loop as a function `fn step(state: AgentState, event: AgentEvent) -> Result<AgentState, TransitionError>` with a separate retry counter on the relevant transitions. This keeps the state set finite and orthogonal: a failed `Thinking → Thinking` retry is a transition annotation, not a fifth state.
   - Implement retry semantics: retryable errors (rate limits, transient HTTP failures) trigger exponential backoff up to a configurable budget; fatal errors (auth failure, malformed schemas) terminate the loop immediately.
   - `Conversation::send` returns `AssistantResponse` which can carry `Vec<ModelToolCall>` even though the Phase 1a loop only acts on the first one. This makes Phase 2's multi-tool-call handling a mechanical change rather than a structural one.
   - The loop looks up the tool in the registry, executes it, appends a `Tool` message with the result, and sends again.
   - Configurable max iterations to prevent infinite loops.
   - Returns when `FinishReason::Stop` or user input is needed.

7. **Fix the assistant tool-call message persistence bug:**
   The current `Conversation::send` builds an `AssistantResponse::ToolCall` from the model's response but **does not push the assistant message into `self.messages` before returning**. When the agent loop comes back with a `Tool` role result and tries to send again, the API will reject the request: a `tool` role message must immediately follow an assistant message that contains the matching `tool_calls` (with matching `tool_call_id`). The fix:
   - On a tool-call response, push the assistant message — including its `tool_calls` field — into history.
   - Then push the `Tool` message(s) with their `tool_call_id` fields when tool results come back.
   - Add a regression test: mock `ChatClient` returns a tool call, the loop executes, the next `ChatRequest` sent to the mock contains both the assistant `tool_calls` message and the `tool` result message in correct order.

8. **Define `ContextManager` as a trait with a sliding-window default:**
   ```rust
   pub trait ContextManager: Send + Sync {
       fn fit(&self, messages: &[ChatMessage], budget: TokenBudget) -> Vec<ChatMessage>;
   }
   ```
   The default `SlidingWindowContextManager`:
   - Pins the system message — it is never evicted.
   - **Evicts by turn, not by individual message.** A "turn" is one of: a single user message; or an assistant message plus its associated `tool_calls` plus all matching `tool` result messages. The window must never split an assistant `tool_calls` message from its matching `tool` results — the API will return 400 if it sees a `tool` message without a preceding assistant `tool_calls` message bound by `tool_call_id`. This invariant is the reason `ContextManager` operates at turn granularity.
   - Approximate token counting is acceptable in Phase 1a (length-based heuristic). Tokeniser-accurate counting can be added later without changing the trait.

   The trait boundary lets Phase 5+ swap in summarisation or retrieval-augmented strategies without touching the agent loop. Whatever strategy is used, the system-message-pinned + no-split-tool-pairs invariants are non-negotiable.

9. **Implement `LocalChatClient`** as the default concrete `ChatClient`. Talks to `localhost` OpenAI-compatible endpoints (LM Studio, Ollama). The existing `RhoHttpClient` collapses into this implementation — it should not survive as a separate abstraction.

10. **Create the `rho-tools` crate.** Implement three minimal tools:
    - `ReadFile` — read a file's text content (risk: `Read`)
    - `WriteFile` — write content to a file (risk: `Write`)
    - `RunCommand` — execute a PowerShell command (risk: `Destructive`)

    These are deliberately minimal stubs in Phase 1a. Sandbox enforcement, command denylist, and approval gating are added in Phase 1b. The tools accept the `CancellationToken` from `Tool::execute`; `RunCommand` wires it to `tokio::process::Child::kill` on cancel.

11. **Update the binary to register tools and run the agent loop.** Minimal slash-command support: `/quit` to exit, `/clear` to reset conversation history. **Phase 1a has no approval prompts** — destructive tools run immediately. This is a known and explicit gap, closed by Phase 1b.

12. **Define the v1 base identity prompt:**
    - The base identity prompt lives at `rho-core/src/prompts/base.md` and is included in the binary at compile time via `include_str!`. Editing the prompt is a Markdown edit that goes through code review like any other change; rebuilding picks it up.
    - Expose it as `pub fn base_prompt() -> &'static str` in a `rho_core::prompts` module (a function rather than a `const` so that runtime substitution — e.g., injecting the current date or OS — can be added later without an API break).
    - The binary uses it as the default system message: `Conversation::new(model, Some(prompts::base_prompt()), tools)` when the user does not pass `--system`. The `--system` CLI flag continues to override (useful for experiments and tests).
    - The v1 draft is stored at `rho-core/src/prompts/base.md`. Its wording is expected to evolve; what matters now is that the file exists at the documented path and that integration tests run against a known baseline rather than an empty system prompt.
    - The prompt deliberately references `<context>` framing and approval-gate behaviour even though those mechanisms are implemented in Phase 1b. The prompt describes the contract; the implementation catches up. Splitting this across phases would fragment the contract for no benefit.
    - Tests to add:
      - `base_prompt()` is non-empty and parses as UTF-8 (compile-time guarantee via `include_str!`, but a sanity test confirms the wiring)
      - The binary, when run without `--system`, places `base_prompt()` as the first message of the conversation
      - The binary, when run with `--system "..."`, uses the provided string instead

13. **Create the `rho-test-helpers` crate with initial contents:**
    - Mock `ChatClient` implementation (returns canned responses for integration tests)
    - Fixture loader helper for `tests/fixtures/`
    - This crate grows incrementally each phase as shared test infrastructure accumulates

14. **Establish the `tests/fixtures/` directory structure and naming convention:**
    - `<crate>/tests/fixtures/<category>/<name>.json` (e.g., `rho-core/tests/fixtures/responses/chat_completion.json`)
    - Fixtures are generated from real API responses where possible (captured during development)
    - This convention is decided now (Phase 1a) to avoid refactoring later

15. **Add integration tests:**
    - Mock `ChatClient` returns tool calls, verify the loop executes and feeds back
    - Verify the assistant `tool_calls` message is persisted before the `tool` result message (regression test for task 7)
    - Verify `ContextManager` does not split assistant `tool_calls` from its `tool` results when sliding the window
    - Verify cancellation propagates from the loop into `Tool::execute`

16. **Add operational tests (security tests are Phase 1b):**
    - Max iteration guard: verify the loop terminates after the configured limit
    - Retry budget: verify exponential backoff is applied to retryable errors and the loop terminates after budget exhaustion

17. **Test suite audit:**
    - Promote shared `ChatClient` mock into `rho-test-helpers`
    - Extract JSON fixtures into `tests/fixtures/` files (avoid inline JSON blobs in test bodies)
    - Ensure every public type has at least a construction/serialization test
    - `ChatMessage` round-trip tests cover both the string-content and array-content wire forms
    - Remove any tests that duplicate coverage without adding value
