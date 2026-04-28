# Phase 1 Tasks

1. **Refine the data model in `rho-core`:**
   - Audit existing types for ergonomics — can they be constructed without boilerplate? Are they composable?
   - Introduce newtypes where they prevent misuse: `FilePath`, `ToolName`, `DiagnosticCode`
   - Decide newtype `Deref` policy explicitly: `FilePath` implements `Deref<Target = Path>`, `ToolName` implements `Deref<Target = str>`. This avoids constant `.0` access at API boundaries while preserving type safety internally. Document the decision in the crate-level doc comment.
   - Ensure every type round-trips through serde (JSON for API, TOML for future config)
   - Add builder-style or `From` impls for common construction patterns
   - Document the design philosophy in the crate-level doc comment

2. **Define the `Tool` trait in `rho-core`:**
   ```rust
   pub trait Tool: Send + Sync {
       fn name(&self) -> ToolName;
       fn description(&self) -> &str;
       fn parameters_schema(&self) -> serde_json::Value;
       async fn execute(&self, arguments: serde_json::Value) -> Result<ToolResult>;
   }
   ```
   The trait uses native `async fn` in traits — Rust 2024 edition supports this natively, so no `async-trait` crate is needed. `rho-core` is an internal kernel, not a published library; the edition guarantee is sufficient. The trait is async from the start because tools like `RunCommand` and `CargoTest` can run for seconds or minutes. A synchronous `execute` would deadlock an async runtime unless every implementation spawns its own thread. A `timeout: Option<Duration>` parameter may be added to the registry invocation to cap long-running tools.

3. **Define `ToolResult` and `ToolRegistry` in `rho-core`.** Tools are registered with a risk level (`Read`, `Write`, `Destructive`) that feeds into the approval policy.

4. **Implement the agent loop in `rho-core`:**
   - Define an `AgentLoopState` enum: `Idle`, `Thinking`, `AwaitingApproval`, `ExecutingTool`, `Error(RhoError)`, `RetryPending`. The loop is a state machine that transitions between these states explicitly.
   - Every tool call passes through the `ApprovalPolicy` before execution. The `AwaitingApproval` state pauses the loop until the UI layer provides a response. The bare REPL handles this with a simple `y/n` prompt.
   - Implement retry semantics: retryable errors (rate limits, transient HTTP failures) trigger exponential backoff up to a configurable budget; fatal errors (auth failure, malformed schemas) terminate the loop immediately.
   - `Conversation::send` returns `AssistantResponse` which can carry `Vec<ModelToolCall>` even though the Phase 1 loop only acts on the first one. This makes Phase 2's multi-tool-call handling a mechanical change rather than a structural one.
   - The loop looks up the tool in the registry, executes it, appends a `Role::Tool` message with the result, and sends again
   - Configurable max iterations to prevent infinite loops
   - Returns when `FinishReason::Stop` or user input is needed

5. **Define the `ChatClient` provider trait in `rho-core`:**
   ```rust
   pub trait ChatClient: Send + Sync {
       async fn chat(&self, request: ChatRequest) -> Result<ModelResponse>;
   }
   ```
   The trait is minimal — one method, provider-agnostic. The agent loop depends on this trait, never on a concrete provider. This serves two purposes:
   - **Testability** — mock implementations return canned responses without a live server
   - **Provider extensibility** — any model provider can be plugged in by implementing the trait
   Implement `LocalChatClient` as the default concrete type (talks to `localhost` OpenAI-compatible endpoints like LM Studio or Ollama). The trait is designed for future streaming support (Phase 4 may add a `chat_stream` method), but starts simple.

6. **Implement a sliding window `ContextManager` in `rho-core`:**
   - Monitor approximate token count in the conversation history
   - Pin the system message — it is never evicted from the window
   - Apply a sliding window over the remaining messages: when approaching the model's context limit, evict the oldest non-system messages to make room for new ones
   - The window maintains a contiguous slice of recent conversation turns, ensuring the model always has the most recent context
   - This prevents hard API errors from overflowing the context window during long agent sessions

7. **Define the `ApprovalPolicy` in `rho-core`:**
   ```rust
   pub trait ApprovalPolicy: Send + Sync {
       fn requires_approval(&self, tool: &ToolName, risk: ToolRisk) -> bool;
   }
   ```
   - Default policy: `Read` tools auto-approved, `Write` and `Destructive` tools require approval
   - The bare REPL renders approval as `Execute [tool_name]? [y/n] `
   - The policy is configurable per-tool via config (Phase 2)
   - This gate exists from day one — even in the bare REPL, the agent cannot execute destructive operations without human confirmation

8. **Implement the file sandbox in `rho-core`:**
   - Define a sandbox root (project directory or explicit `--root` argument)
   - `FilePath` canonicalises the path (resolving `..`, symlinks, junctions on Windows) and validates it's within the root
   - File tools (`ReadFile`, `WriteFile`, `EditFile`, `ListDir`) refuse paths outside the sandbox root
   - Configurable opt-out: `sandbox = false` in config disables the check (user assumes responsibility)

9. **Add `Role::Context` to `rho-core`:**
   - A distinct role for file contents and tool output that enters the conversation, separate from `Role::User`
   - The system prompt instructs the model to treat `Role::Context` messages as data, not instructions — a defense-in-depth measure against prompt injection via file contents
   - Tool results and `ReadFile` output use `Role::Context`; user messages use `Role::User`

10. **Implement project context file scanning in `rho-core`:**
    - Define a default scan list of filenames: `AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`
    - At startup, scan the sandbox root for these files. For each found file, load its contents and incorporate them into the system prompt composition
    - The scan list is configurable in `.rho/config.toml` (Phase 2 adds the config loader; until then, the default list is hardcoded)
    - All project context files go through the trust model: hash verification, user confirmation on first load, re-confirmation if the file changes (trust storage comes in Phase 5; until then, a simple first-load confirmation in the REPL suffices)
    - Project context file contents are appended to the system prompt as clearly delimited sections (e.g., `--- AGENTS.md ---`), not as `Role::Context` messages — these are intentional instructions the user placed in the project, not untrusted data
    - Precedence: base identity prompt → project context files (in scan-list order) → tool schemas. This ensures the agent's core identity is always first and cannot be overridden by a context file.

11. **Create the `rho-tools` crate.** Implement three minimal tools:
    - `ReadFile` — read a file's text content (risk: `Read`)
    - `WriteFile` — write content to a file (risk: `Write`)
    - `RunCommand` — execute a PowerShell command (risk: `Destructive`)

12. **Create the `rho-highlight` crate with initial scaffolding:**
    - Add `tree-sitter` and `tree-sitter-rust` as dependencies
    - Implement a `parse` function that takes source text and returns a tree-sitter `Tree`
    - Implement a `highlight` function that produces ANSI-highlighted output for Rust source
    - Define Cargo features from the start: `rust` as default, with `powershell`, `toml`, `json`, `markdown` as opt-in features. This avoids a painful retrofit in Phase 4 when additional grammars are added.
    - This is minimal — just enough to validate the integration path. Richer grammars and queries come in Phase 4.

13. **Update the binary to register tools and run the agent loop.** Add minimal slash-command support in the bare REPL: `/quit` to exit, `/clear` to reset conversation history. The approval gate renders as `y/n` prompts for destructive tools. On startup, scan for project context files and confirm trust for any found. Full slash-command parsing and rendering is deferred to Phase 4 (TUI).

14. **Create the `rho-test-helpers` crate with initial contents:**
    - Mock `ChatClient` implementation (returns canned responses for integration tests)
    - Fixture loader helper for `tests/fixtures/`
    - This crate grows incrementally each phase as shared test infrastructure accumulates

15. **Establish the `tests/fixtures/` directory structure and naming convention:**
    - `<crate>/tests/fixtures/<category>/<name>.json` (e.g., `rho-core/tests/fixtures/responses/chat_completion.json`)
    - Fixtures are generated from real API responses where possible (captured during development)
    - This convention is decided now (Phase 1) to avoid refactoring later

16. **Add integration tests:** mock `ChatClient` returns tool calls, verify the loop executes and feeds back.

17. **Add security tests:**
    - Approval policy: verify destructive tools require approval, read tools auto-approve
    - File sandbox: verify `FilePath` rejects paths outside the sandbox root (including `..` traversal, symlinks, junctions)
    - Secret redaction: verify common secret patterns (API keys, tokens) are replaced with `[REDACTED]`
    - Max iteration guard: verify the loop terminates after the configured limit
    - Project context file trust: verify untrusted files require confirmation, verify changed files require re-confirmation

18. **Test suite audit:**
    - Promote shared `ChatClient` mock into `rho-test-helpers`
    - Extract JSON fixtures into `tests/fixtures/` files (avoid inline JSON blobs in test bodies)
    - Ensure every public type has at least a construction/serialization test
    - Remove any tests that duplicate coverage without adding value
