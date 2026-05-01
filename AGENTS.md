# AGENTS.md

Guidance for AI assistants working on this codebase.

## Quick Start

```sh
cargo xtask ci    # Run the full CI pipeline before considering work done
```

This runs `fmt → lint → build → test` in sequence. All four must pass.

## Project Layout

```
rho/                # Binary entry point (`rho` CLI)
  src/
    main.rs         # REPL loop, tool wiring, CLI dispatch
rho-core/           # Core library
  src/
    lib.rs          # Module declarations and convenience re-exports
    agent.rs        # Agent loop state machine and `run_loop`
    approval.rs     # `ApprovalPolicy` and `ApprovalGate` traits
    client.rs       # `ChatClient` trait, `LocalChatClient`
    config.rs       # `RhoConfig`, `ConfigLoader`, config sub-types — two-tier TOML loading
    context.rs      # `ContextManager` trait, `SlidingWindowContextManager`, `TokenBudget`
    context_files.rs# Project context file scanner, `TrustStore`, prompt composition
    conversation.rs # `Conversation`, `AssistantResponse`
    error.rs        # `RhoError` and `Result`
    message.rs      # `ChatMessage`, `ContentBlock`, `ModelToolCall`
    newtypes.rs     # `FilePath`, `ToolName`, `ToolCallId`, `DiagnosticCode`
    prompts.rs      # `base_prompt()`, `compact_prompt()` (embedded from `prompts/base.md`, `prompts/compact.md`)
    redact.rs       # `Redactor` — secret pattern matching
    request.rs      # `ChatRequest`
    response.rs     # `ModelResponse`, `FinishReason`, `ModelUsage`
    sandbox.rs      # `SandboxRoot` — file sandbox validation
    schema.rs       # `ToolSchema` — wire-format tool definitions
    shell.rs        # `ShellExecutor` trait, `ShellOutput` — shell execution abstraction
    tool.rs         # `Tool` trait, `ToolRegistry`, `ToolResult`, `ToolRisk`, `CancellationToken`
rho-tools/          # Built-in tool implementations
  src/
    lib.rs          # `register_all()`
    files.rs        # `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
    shell.rs        # `PowerShellExecutor`, `RunCommand` (delegates to `ShellExecutor` trait)
rho-test-helpers/   # Shared test infrastructure (dev-only)
  src/
    lib.rs          # `MockChatClient`, response builders, sandbox/trust helpers
xtask/              # Dev task runner
  src/
    main.rs         # CLI dispatch
    tasks.rs        # Task implementations
Cargo.toml          # Workspace root
CHANGELOG.md        # Generated via git-cliff
cliff.toml          # git-cliff configuration
```

## Architecture

`rho` is a local coding agent that communicates with OpenAI-compatible model APIs.
The workspace is layered — dependencies flow downward only:

```
rho (binary) → rho-tools → rho-core
                  ↓
             rho-test-helpers → rho-core
```

The core flow is:

1. **`ToolRegistry`** holds `Box<dyn Tool>` implementations, each with a name, schema, and risk level.
2. **`Conversation`** accumulates `ChatMessage`s with an optional system prompt and tool schemas.
3. **`run_loop`** (in `agent.rs`) drives the interaction: send to model → if tool calls, get approval for each → execute each tool sequentially → feed results back → repeat until the model returns a text reply.
4. **`ChatClient`** trait abstracts the model API. `LocalChatClient` targets `localhost:1234` (LM Studio, Ollama).

### Safety layers

The agent uses multiple defense-in-depth layers:

- **File sandbox** — `SandboxRoot` validates all file paths stay within the project root.
- **Approval gate** — `ApprovalPolicy` decides which tools need confirmation; `ApprovalGate` asks the user.
- **Secret redaction** — `Redactor` replaces known secret patterns in tool output with `[REDACTED]`.
- **Untrusted-data framing** — File contents from tools are wrapped in `<context>` tags to resist prompt injection.
- **Context-file trust** — `TrustStore` verifies SHA-256 hashes of project instruction files (`AGENTS.md`, etc.).
- **Retry with backoff** — Transient HTTP errors are retried with exponential backoff up to a configurable budget.
- **Max iteration guard** — The agent loop terminates after a configurable number of tool-call rounds.

## Key Types

| Type | Purpose |
|---|---|
| `ChatMessage` | Conversation message (`System`/`User`/`Assistant`/`Tool` variants with `Vec<ContentBlock>`) |
| `ContentBlock` | Typed content within a message (currently `Text` only) |
| `ModelToolCall` | Tool invocation requested by the model |
| `ToolCallFunction` | Function name and JSON-encoded arguments within a `ModelToolCall` |
| `ToolSchema` | Wire-format tool definition sent to the model API |
| `ChatRequest` | Request body sent to the model API (`model`, `messages`, `tools`) |
| `ModelResponse` | Parsed API response |
| `ModelChoice` | A single completion choice |
| `FinishReason` | Why the model stopped (`Stop`, `ToolCalls`, `Length`, `ContentFilter`) |
| `ModelUsage` | Token usage statistics |
| `Tool` | Trait all tools implement (`name`, `description`, `schema`, `risk`, `execute`) |
| `ToolRegistry` | Maps tool names to `Box<dyn Tool>` implementations |
| `ToolRisk` | Risk classification: `Read`, `Write`, `Destructive` |
| `ToolResult` | Immediate result of a tool execution (`output`, `is_error`) |
| `ToolOutcome` | `Immediate(ToolResult)` or `Streamed(Receiver)` (streaming lands in Phase 4) |
| `CancellationToken` | Cooperative cancellation signal (re-exported from `tokio_util::sync`) |
| `ShellExecutor` | Trait: interface for executing shell commands |
| `ShellOutput` | Structured shell output: `stdout`, `stderr`, `exit_code` |
| `PowerShellExecutor` | Default `ShellExecutor` implementation (lives in `rho-tools`) |
| `CommandDenylist` | Dangerous command denylist for `RunCommand` (lives in `rho-tools`) |
| `ListDir` | `.gitignore`-aware directory listing tool (lives in `rho-tools`) |
| `EditFile` | Exact-match file editing tool (lives in `rho-tools`) |
| `ApprovalPolicy` | Trait: decides whether a tool call needs human confirmation |
| `ApprovalGate` | Trait: asks the user for confirmation at runtime |
| `ConfigApprovalPolicy` | Config-driven approval: per-tool overrides (`Auto`/`Ask`/`Deny`) with risk-based fallback |
| `ApprovalAction` | Per-tool approval action: `Auto`, `Ask`, `Deny` |
| `ChatClient` | Trait: sends `ChatRequest` and returns `ModelResponse` |
| `LocalChatClient` | Default `ChatClient` for localhost OpenAI-compatible endpoints |
| `Conversation` | Message history + model ID + tool schemas + context manager |
| `AssistantResponse` | `Message(String)` or `ToolCalls(Vec<ModelToolCall>)` |
| `AgentConfig` | Loop settings: max iterations, retry budget, backoff, approval policy |
| `AgentState` | Observable state: `Idle`, `Thinking`, `AwaitingApproval`, `ExecutingTool` |
| `ContextManager` | Trait: fits messages within a token budget |
| `SlidingWindowContextManager` | Default implementation — evicts by turn, pins system message |
| `TokenBudget` | Max tokens for context window |
| `SandboxRoot` | Canonical root for file sandbox validation |
| `find_project_root` | Auto-detect project root by walking up from CWD looking for markers |
| `ModelInfo` | A model returned by the `/v1/models` endpoint |
| `ModelList` | Response from `/v1/models` — a list of [`ModelInfo`] |
| `Redactor` | Best-effort secret pattern scanner |
| `RhoConfig` | Merged application-wide config from project-level + user-level TOML |
| `ConfigLoader` | Loads and merges `~/.rho/config.toml` + `.rho/config.toml` |
| `ProviderConfig` | Model provider selection and connection settings |
| `ApprovalConfig` | Per-tool approval policy overrides |
| `ShellConfig` | Command denylist extensions from config |
| `SandboxConfig` | Sandbox on/off toggle |
| `ContextConfig` | Project context file scan list override |
| `EgressConfig` | Network egress allowlist |
| `RedactionConfig` | Secret redaction on/off toggle |
| `SystemPromptConfig` | System prompt extension fragments |
| `FilePath` | Newtype for sandboxed file paths (`Deref<Target = Path>`) |
| `ToolName` | Newtype for tool names (`Deref<Target = str>`) |
| `ToolCallId` | Newtype for model-issued tool call IDs (`Deref<Target = str>`) |
| `DiagnosticCode` | Newtype for Rust compiler diagnostic codes (`Deref<Target = str>`) |
| `RhoError` | Error enum: `Http`, `HttpError`, `Json`, `ToolNotFound`, `MaxIterationsExceeded`, `RetryBudgetExhausted`, `Unexpected` |
| `Result` | `std::result::Result<T, RhoError>` |

## Coding Conventions

- **Edition:** Rust 2024.
- **Clippy:** Pedantic + cargo lints are enforced (`-D warnings`). Missing docs on private items and error variants are warnings.
- **Formatting:** `cargo fmt --all -- --check` must pass.
- **Commits:** [Conventional commits](https://www.conventionalcommits.org/) — `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- **Versioning:** Bump the version in the workspace `Cargo.toml` only. It propagates via `workspace = true`.

## Testing

```sh
cargo xtask test                # Run all tests
cargo xtask test -- --nocapture # Run with stdout visible
```

- Unit tests live in `#[cfg(test)] mod tests` blocks within each source file.
- Integration tests live in `rho-core/tests/integration_tests.rs`.
- Tool integration tests live in `rho-tools/tests/tool_tests.rs`.
- `rho-test-helpers` provides `MockChatClient`, `MockShellExecutor`, response builders (`text_response`, `tool_call_response`, `multi_tool_call_response`), approval gates (`AutoApproveGate`, `AutoDenyGate`), sandbox helpers (`tempdir_with_sandbox`), and trust-store helpers (`empty_trust_store`).
- When adding new deserialization logic, add a JSON fixture test.
- For `Conversation` branching logic, prefer the trait-abstraction pattern over coupling to `LocalChatClient`.

## Release Checklist

1. Ensure `cargo xtask ci` passes.
2. `cargo xtask changelog <version>` — update `CHANGELOG.md`.
3. Bump `version` in workspace `Cargo.toml`.
4. `git add -A && git commit -m "chore(release): prepare <version>"`.
5. `git push origin trunk`.
