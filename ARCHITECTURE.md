# Architecture

## Overview

`rho` is a local coding agent that communicates with OpenAI-compatible model APIs. The workspace is a layered Rust crate structure where dependencies flow downward only.

## Dependency Graph

```
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │  ← Assembles all layers, runs the app
├─────────────────────────────────────────────────┤
│                   rho-tools                      │  ← Built-in tool implementations
├─────────────────────────────────────────────────┤
│                   rho-highlight                  │  ← Tree-sitter syntax analysis
├─────────────────────────────────────────────────┤
│                   rho-core                       │  ← Agent kernel (loop, types, traits, data model)
└─────────────────────────────────────────────────┘

   ┌──────────────────┐  ┌──────────────────┐
   │ rho-test-helpers  │  │    rho-eval       │  ← Dev-only: mocks, fixtures, benchmarks
   └──────────────────┘  └──────────────────┘

                  rho-bench → rho-eval → rho-core
                  rho-tools → rho-highlight → rho-core
                  rho-test-helpers → rho-core
```

**Rule:** a crate may only depend on crates below it in the stack. `rho-core` depends on nothing but external libraries.

## Crate Responsibilities

### `rho-core` — Agent Kernel

The foundation. Defines the contract everything else implements.

- **Agent loop** — `run_loop()` drives the interaction: send to model → if tool calls, get approval → execute tools → feed results back → repeat until text reply.
- **Data model** — `ChatMessage` (variant per role: `System`/`User`/`Assistant`/`Tool`), `ContentBlock`, `ModelToolCall`, `AssistantResponse`.
- **Tool trait** — `Tool`: async, dyn-compatible, takes `CancellationToken`, returns `ToolOutcome`.
- **Tool registry** — Maps tool names to `Box<dyn Tool>` implementations, each with a risk level.
- **Approval policy** — `ApprovalPolicy` decides whether a tool call needs confirmation; `ApprovalGate` asks the user.
- **Chat client** — `ChatClient` trait abstracts the model API. `LocalChatClient` is the OpenAI-compatible implementation.
- **Session** — Tree-shaped conversation model with adaptive resolution, JSONL persistence, token estimation.
- **Config** — `RhoConfig` merged from user-level (`~/.rho/config.toml`) and project-level (`.rho/config.toml`).
- **File sandbox** — `SandboxRoot` validates all file paths stay within the project root.
- **Secret redaction** — `Redactor` replaces known secret patterns in tool output.
- **Context management** — `SlidingWindowContextManager` keeps the conversation within the model's context limit.

`rho-core` does **not** know about:
- PowerShell, shells, or any specific command execution
- File systems (beyond sandbox root validation)
- Terminal rendering
- How approval prompts are rendered (it decides *whether* to approve; the UI decides *how* to ask)

### `rho-tools` — Built-in Tool Implementations

Concrete tools that ship with the agent:

| Tool | Description |
|---|---|
| `ReadFile` | Read a file's contents (text or image) |
| `WriteFile` | Create or overwrite a file |
| `EditFile` | Apply targeted replacements (exact-match) |
| `ListDir` | List directory contents (`.gitignore`-aware) |
| `RunCommand` | Execute a PowerShell command, capture output |
| `CargoCheck` | Run `cargo check`, return parsed diagnostics |
| `CargoClippy` | Run `cargo clippy`, return parsed lints |
| `CargoTest` | Run `cargo test`, return structured results |
| `CargoFix` | Apply machine-applicable suggestions |
| `RustcExplain` | Run `rustc --explain <CODE>` |

Security controls: command denylist, sandbox-scoped working directory, structured output capture.

### `rho-highlight` — Tree-Sitter Syntax Analysis

Syntax highlighting and structural code awareness. Serves two roles:

1. **Structural understanding** — Tools use tree-sitter to reason about code (find function boundaries, validate edit regions).
2. **Rendering** — The TUI uses tree-sitter to syntax-highlight code blocks and diffs.

Currently ships the Rust grammar. PowerShell, TOML, and JSON grammars are future additions.

### `rho` — Binary Entry Point

Assembles all layers:

1. Parse CLI arguments
2. Load config (two-tier TOML)
3. Resolve provider and model
4. Scan for project context files, verify trust
5. Create tool registry and register built-in tools
6. Construct session (persisted, resumed, or ephemeral)
7. Start the agent loop connected to REPL or TUI

### `rho-test-helpers` — Shared Test Utilities (dev-only)

`MockChatClient`, `MockShellExecutor`, response builders, approval gates, file-system test environment, sandbox/trust helpers.

### `rho-eval` — Behavioural Benchmarks (dev-only)

Canonical coding tasks with known correct outcomes. Defines `EvalTask` trait, scoring logic, and regression detection.

### `rho-bench` — Benchmark Harness (dev-only)

CLI tool that runs eval tasks against models with timing and token metrics.

## Core Flow

```
1. ToolRegistry holds Box<dyn Tool> implementations
2. Session accumulates ChatMessages with system prompt and tool schemas
3. run_loop() drives the interaction:
   ┌──────────────────────────────────────────────┐
   │  Send request to model (via ChatClient)      │
   │         ↓                                     │
   │  Response has tool calls?                     │
   │    Yes → Get approval for each tool call      │
   │         → Execute each tool sequentially      │
   │         → Feed results back as Tool messages  │
   │         → Loop back to "Send request"         │
   │    No  → Return text reply to user            │
   └──────────────────────────────────────────────┘
```

## Safety Layers

The agent uses defense-in-depth — no single layer is sufficient, but each raises the bar:

| Layer | Mechanism | Purpose |
|---|---|---|
| File sandbox | `SandboxRoot` | Validates file paths stay within project root |
| Approval gate | `ApprovalPolicy` + `ApprovalGate` | User confirms destructive actions |
| Secret redaction | `Redactor` | Replaces known secret patterns in tool output |
| Untrusted-data framing | `<context>` wrapper | Resists prompt injection from file contents |
| Context-file trust | `TrustStore` + SHA-256 | Verifies project instruction files haven't changed |
| Command denylist | `CommandDenylist` | Blocks dangerous shell commands |
| Retry with backoff | Exponential backoff | Handles transient HTTP errors |
| Max iteration guard | Configurable limit | Prevents infinite tool-call loops |
| Provider consent | `check_provider_consent()` | Warns before sending data to external servers |

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
    client.rs       # `ChatClient` trait, `LocalChatClient`, `client_factory()`
    config.rs       # `RhoConfig`, `ConfigLoader`, config sub-types
    context.rs      # `ContextManager` trait, `SlidingWindowContextManager`, `TokenBudget`
    context_files.rs# Project context file scanner, `TrustStore`
    conversation.rs # `Conversation`, `AssistantResponse`
    diagnostic.rs   # `Diagnostic`, `DiagnosticSeverity`, `DiagnosticSpan`
    error.rs        # `RhoError` and `Result`
    message.rs      # `ChatMessage`, `ContentBlock`, `ModelToolCall`
    newtypes.rs     # `FilePath`, `ToolName`, `ToolCallId`, `DiagnosticCode`
    prompts.rs      # `base_prompt()`, `compact_prompt()`
    redact.rs       # `Redactor` — secret pattern matching
    request.rs      # `ChatRequest`
    response.rs     # `ModelResponse`, `FinishReason`, `ModelUsage`
    sandbox.rs      # `SandboxRoot` — file sandbox validation
    schema.rs       # `ToolSchema` — wire-format tool definitions
    session.rs      # `Session` — tree-shaped conversation with JSONL persistence
    shell.rs        # `ShellExecutor` trait, `ShellOutput`
    stream.rs       # `StreamChunk`, SSE parsing, accumulation
    tool.rs         # `Tool` trait, `ToolRegistry`, `ToolResult`, `ToolRisk`
rho-tools/          # Built-in tool implementations
  src/
    lib.rs          # `register_all()`
    files.rs        # `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
    shell.rs        # `PowerShellExecutor`, `RunCommand`
    rust.rs         # `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
rho-highlight/      # Tree-sitter syntax analysis
  src/
    lib.rs          # Re-exports
    parse.rs        # Tree-sitter parsing
    highlight.rs    # Token classification and highlighting
    query.rs        # AST node lookup by position
rho-eval/           # Behavioural benchmark definitions (dev-only)
  src/
    lib.rs          # `EvalTask`, `TaskOutcome`, `TaskMetrics`, `EvalRun`
    task.rs         # `EvalTask` trait, `TaskVerdict`
    report.rs       # `EvalRun`, `EvalReport`
    tasks.rs        # Built-in task definitions
rho-bench/          # Benchmark harness binary (dev-only)
  src/
    main.rs         # CLI
    harness.rs      # `CountingClient`, `BenchApprovalGate`
    comparison.rs   # Terminal table display
    persistence.rs  # JSON result files
rho-test-helpers/   # Shared test infrastructure (dev-only)
  src/
    lib.rs          # `MockChatClient`, response builders, helpers
xtask/              # Dev task runner
  src/
    main.rs         # CLI dispatch
    tasks.rs        # Task implementations
Cargo.toml          # Workspace root
CHANGELOG.md        # Generated via git-cliff
cliff.toml          # git-cliff configuration
```

## Key Types

### Messages and requests

| Type | Location | Purpose |
|---|---|---|
| `ChatMessage` | `message.rs` | Conversation message (`System`/`User`/`Assistant`/`Tool`) |
| `ContentBlock` | `message.rs` | Typed content within a message (`Text`) |
| `ModelToolCall` | `message.rs` | Tool invocation requested by the model |
| `ChatRequest` | `request.rs` | Request body sent to the model API |
| `ModelResponse` | `response.rs` | Parsed API response |
| `FinishReason` | `response.rs` | Why the model stopped (`Stop`, `ToolCalls`, `Length`, etc.) |
| `AssistantResponse` | `conversation.rs` | `Message(String)` or `ToolCalls(Vec<ModelToolCall>)` |

### Tools

| Type | Location | Purpose |
|---|---|---|
| `Tool` | `tool.rs` | Trait: `name`, `description`, `schema`, `risk`, `execute` |
| `ToolRegistry` | `tool.rs` | Maps tool names to `Box<dyn Tool>` |
| `ToolRisk` | `tool.rs` | `Read`, `Write`, `Destructive` |
| `ToolResult` | `tool.rs` | Result of a tool execution |
| `ToolSchema` | `schema.rs` | Wire-format tool definition |
| `CancellationToken` | `tool.rs` | Cooperative cancellation signal |

### Client and provider

| Type | Location | Purpose |
|---|---|---|
| `ChatClient` | `client.rs` | Trait: sends `ChatRequest`, returns `ModelResponse` |
| `LocalChatClient` | `client.rs` | OpenAI-compatible HTTP client |
| `ModelInfo` | `client.rs` | A model from `/v1/models` |
| `ProviderConfig` | `config.rs` | Endpoint and API key env var configuration |

### Session and context

| Type | Location | Purpose |
|---|---|---|
| `Session` | `session.rs` | Tree-shaped conversation with JSONL persistence |
| `ContextManager` | `context.rs` | Trait: fits messages within a token budget |
| `SlidingWindowContextManager` | `context.rs` | Evicts by turn, pins system message |
| `TokenBudget` | `context.rs` | Context window + completion reserve |

### Safety

| Type | Location | Purpose |
|---|---|---|
| `SandboxRoot` | `sandbox.rs` | File sandbox validation |
| `ApprovalPolicy` | `approval.rs` | Decides if a tool call needs confirmation |
| `ApprovalGate` | `approval.rs` | Asks the user for confirmation |
| `Redactor` | `redact.rs` | Secret pattern matching |
| `TrustStore` | `context_files.rs` | SHA-256 hash verification for context files |

### Configuration

| Type | Location | Purpose |
|---|---|---|
| `RhoConfig` | `config.rs` | Merged application-wide config |
| `ConfigLoader` | `config.rs` | Two-tier TOML loading |
| `AgentConfig` | `agent.rs` | Loop settings: iterations, retry, backoff |

### Errors

| Type | Location | Purpose |
|---|---|---|
| `RhoError` | `error.rs` | `Http`, `HttpError`, `Json`, `ToolNotFound`, `MaxIterationsExceeded`, `RetryBudgetExhausted`, `Cancelled`, `Unexpected` |
| `Result` | `error.rs` | `std::result::Result<T, RhoError>` |

### Newtypes

| Type | Location | Purpose |
|---|---|---|
| `FilePath` | `newtypes.rs` | Sandboxed file paths |
| `ToolName` | `newtypes.rs` | Tool names |
| `ToolCallId` | `newtypes.rs` | Model-issued tool call IDs |
| `DiagnosticCode` | `newtypes.rs` | Rust compiler diagnostic codes |

### Dev-only (rho-eval / rho-bench)

| Type | Location | Purpose |
|---|---|---|
| `EvalTask` | `rho-eval` | Trait: benchmark task with verification |
| `TaskVerdict` | `rho-eval` | `Pass`, `Fail`, `Error` |
| `TaskOutcome` | `rho-eval` | Verdict + metrics |
| `EvalRun` | `rho-eval` | Collection of outcomes with regression detection |

## Platform Support

Windows, macOS, Linux. PowerShell 7+ (`pwsh`) is the primary shell on all platforms; Windows PowerShell 5.1 (`powershell`) is the fallback on Windows only. Path normalization is platform-aware.
