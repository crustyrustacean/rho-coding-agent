# Architecture

## Overview

`rho` is a local coding agent that communicates with OpenAI-compatible model APIs. The workspace is a layered Rust crate structure where dependencies flow downward only.

## Dependency Graph

The crates form layers; dependencies flow downward only. Crates at the same
level are **siblings** — they depend on layers below, not on each other
(e.g. `rho-ext` does **not** depend on `rho-tools`).

```
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │  ← Headless JSON-RPC 2.0 agent
├─────────────────────────────────────────────────┤
│  rho-ext          rho-tools                       │  ← Extensions   /  Built-in tools
├─────────────────────────────────────────────────┤
│            rho-memory      rho-highlight           │  ← Siblings of rho-tools
├─────────────────────────────────────────────────┤
│                   rho-core                       │  ← Agent kernel (loop, types, traits, data model)
├─────────────────────────────────────────────────┤
│                   rho-ai                         │  ← Unified LLM provider abstraction
└─────────────────────────────────────────────────┘

   ┌──────────────────┐
   │ rho-test-helpers  │  ← Dev-only: mocks, fixtures (depends on rho-core)
   └──────────────────┘
```

`rho-ext`, `rho-tools`, `rho-memory`, and `rho-highlight` are all siblings
sitting directly on `rho-core`. `rho-tools` additionally depends on
`rho-memory` and `rho-highlight`; the others each depend only on `rho-core`.

**Actual dependency edges:**

- `rho` → `rho-core`, `rho-tools`, `rho-ext`, `rho-ai`
- `rho-ext` → `rho-core`
- `rho-tools` → `rho-core`, `rho-memory`, `rho-highlight`
- `rho-memory` → `rho-core`
- `rho-highlight` → `rho-core`
- `rho-core` → `rho-ai` (for the `LlmService` trait)
- `rho-test-helpers` → `rho-core`



## Crate Responsibilities

### `rho-core` — Agent Kernel

The foundation. Defines the contract everything else implements.

- **Agent loop** — `run_loop()` drives the interaction: send to model → if tool calls, get approval → execute tools → feed results back → repeat until text reply. Returns [`AgentResult`] with the reply text, tool call history, token usage, iteration count, duration, finish reason, and context stats. A [`CollectingObserver`] records tool call events internally; no custom observer is needed by consumers.
- **Data model** — `ChatMessage` (variant per role: `System`/`User`/`Assistant`/`Tool`), `ContentBlock`, `ModelToolCall`, `AssistantResponse`.
- **Tool trait** — `Tool`: async, dyn-compatible, takes `CancellationToken`, returns `ToolOutcome`.
- **Tool registry** — Maps tool names to `Box<dyn Tool>` implementations, each with a risk level.
- **Approval policy** — `ApprovalPolicy` decides whether a tool call needs confirmation; `ApprovalGate` asks the user.
- **LLM service** — `LlmService` trait (from `rho-ai`) abstracts the model API. `RhoAiClient` wraps it for use in the agent loop. `ProviderRegistry` manages one or more providers with CLI endpoint/key overrides. Provider-aware model resolution ensures `/model` switches to the correct provider automatically.
- **Session** — Tree-shaped conversation model with adaptive resolution, JSONL persistence, token estimation.
- **Config** — `RhoConfig` merged from user-level (`~/.rho/config.toml`) and project-level (`.rho/config.toml`). Supports `[[providers]]` array with named presets (`lm-studio`, `ollama`, `openrouter`, `openai`, `groq`, `zai`). Project-level providers merge with user-level by name — same name overrides, new names are added, unmatched user providers are preserved.
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
| `BatchRead` | Read multiple files at once (up to 20 paths) in a single call |
| `WriteFile` | Create or overwrite a file |
| `EditFile` | Apply targeted replacements (exact-match) |
| `ListDir` | List directory contents (`.gitignore`-aware) |
| `RunCommand` | Execute a PowerShell command, capture output |
| `CargoCheck` | Run `cargo check`, return parsed diagnostics |
| `CargoClippy` | Run `cargo clippy`, return parsed lints |
| `CargoTest` | Run `cargo test`, return structured results |
| `CargoFix` | Apply machine-applicable suggestions |
| `RustcExplain` | Run `rustc --explain <CODE>` |
| `RustdocTool` | Look up Rust standard-library docs from the locally installed rustdoc |
| `CratesIoLookup` | Look up crates on crates.io (search, info, versions, downloads) |
| `SessionSummary` | Read the session's compressed turn history for context recovery |
| `MemoryTool` | Store, search, and manage project-local knowledge (via `rho-memory`) |

Security controls: command denylist, sandbox-scoped working directory, structured output capture.

### `rho-highlight` — Tree-Sitter Syntax Analysis

Syntax highlighting and structural code awareness. Serves two roles:

1. **Structural understanding** — Tools use tree-sitter to reason about code (find function boundaries, validate edit regions).
2. **Rendering** — Tools and extensions use tree-sitter to syntax-highlight code blocks and diffs.

Currently ships the Rust grammar. PowerShell, TOML, and JSON grammars are future additions.

### `rho-ai` — Unified LLM Provider Abstraction

Provider-agnostic streaming interface for LLM communication.

- **`LlmService` trait** — `chat_stream(LlmRequest) → EventStream`. All providers implement this.
- **`OpenAiService`** — OpenAI-compatible HTTP client with SSE parsing and retry logic. Covers OpenAI, DeepSeek, OpenRouter, Groq, local servers (Ollama, LM Studio), and any OpenAI-compatible endpoint.
- **Unified types** — `LlmMessage`, `LlmRequest`, `ToolDefinition`, `StreamEvent` (`Text`, `Reasoning`, `ToolUseStart/Delta/Complete`, `Done`), `AccumulatedResponse`.
- **Model catalog** — `Catalog` (`catalog.rs`) is a built-in model registry generated from OpenRouter's public model list by `cargo xtask generate-models` (output: `catalog_generated.rs`), with manual overrides for thinking support and compatibility flags in `model-overrides.json`. `Model` entries carry context-window limits, thinking support, and `ModelCost` (per-million-token pricing for input, output, cache-read, cache-write). `Catalog::find`/`search`/`by_provider` resolve models for cost computation and `listModels`. Users extend the catalog with custom models via `[[models]]` provider config.
- **Retry** — Exponential backoff on transient errors with configurable budget.
- **SSE parser** — Line-buffered server-sent event parser with streaming accumulation.


### `rho-ext` — TypeScript Extension Runtime

Enables user-authored TypeScript extensions that add tools, hooks, and commands to rho without modifying the core codebase.

- **`ExtensionRuntime`** — owns a V8 isolate on a dedicated OS thread. Supports async calls for tools, hooks, and commands via tokio channels.
- **`ExtensionLoader`** — orchestrates discovery, filtering (config-driven), spawning, tool registration, and hot reload (mtime-based change detection).
- **`DenoTool`** — wraps extension tool functions as `Box<dyn Tool>` for the `ToolRegistry`.
- **`DenoObserver`** — wraps extension hooks as `AgentObserver` for agent-loop event interception.
- **`CompositeObserver`** — fans out agent-loop events to RPC observer + extension observers. First `Block` wins for tool-call interception.
- **TypeScript transpilation** — `deno_ast` transpiles `.ts` to JS at load time (no external Deno CLI needed).
- **Host functions** — `rho.log()`, `rho.readFile()`, `rho.writeFile()`, `rho.runCommand()`, `rho.getModel()`, `rho.getCwd()` with permission gating.
- **Config** — `[extensions]` section in config.toml with enabled/disabled allowlists and per-extension permissions.
- **Type definitions** — `rho-ext/types/rho.d.ts` for extension author IntelliSense.

Extension directories: `~/.rho/extensions/` (user-level) and `.rho/extensions/` (project-level). Project-local extensions override user-level ones with the same name.


### `rho-memory` — Persistent Knowledge Base

Project-local knowledge storage with full-text search, content deduplication, and soft deletes over `SQLite` (FTS5). Exposed to the agent via the `MemoryTool` in `rho-tools`.

- **`Memory`** — public API wrapping a `Database` handle. Opens a file-backed database at `.rho/memory.db` within the project root.
- **`Database`** — raw `SQLite` operations: CRUD, FTS5 search, pagination, stats.
- **`Document`** — stored document model (id, title, content, content_hash, tags, metadata, timestamps).
- **`MemoryTool`** — `Tool` trait implementation in `rho-tools` providing `store`, `search`, `get`, `update`, `delete`, `list` operations for the LLM.
- **Config** — `[memory]` section in `config.toml` with `enabled` gate (default: `false`).
- **Storage** — project-local at `<project-root>/.rho/memory.db`.
### `rho` — Headless JSON-RPC 2.0 Agent

Assembles all layers and runs the headless JSON-RPC 2.0 protocol over stdin/stdout. Wire-format types live in `rpc_wire.rs` (typed structs for every method param, result, and notification), and the dispatch loop in `rpc.rs` deserializes params and serializes results through them. An [`OpenRPC` schema](../docs/rpc-schema/openrpc.json) documents the protocol for client generation.

**Startup sequence (`App::build`):**
1. Parse CLI arguments (`Cli` — `--model`, `--endpoint`, `--api-key-env`, session flags, etc.)
2. Load config (two-tier TOML)
3. Construct `ProviderRegistry` from config + CLI overrides
4. Check provider type compatibility and external provider consent (`--accept-external-provider` required for external endpoints)
5. Create tool registry and register built-in tools
6. Discover and load TypeScript extensions via `ExtensionLoader` (spawn V8 isolates, register extension tools)
7. Build `CompositeObserver` from RPC observer + extension `DenoObserver`s
8. Scan context files (headless policy: auto-deny new/changed files)
9. Compose system prompt (base + trusted context files + environment block)
10. Resolve model identifier from config (`agent.model`, `agent.provider`, provider `default_model`, or `--model`). No network calls at startup.
11. Construct session (persisted, resumed, or ephemeral)
12. Fire extension `onLoad` hooks

**`App::run`:** fires extension `onLoad` hooks, then starts the JSON-RPC 2.0 loop via `run_rpc`.

## Execution Mode: JSON-RPC 2.0

`rho` runs as a headless agent communicating via **JSON-RPC 2.0** over stdin/stdout. All requests must include `"jsonrpc": "2.0"`, a `method` field, optional `params`, and a numeric or string `id` for response correlation. Streaming events are delivered as JSON-RPC notifications (no `id` field).

Diagnostic output (warnings, budget info, session status) is written to **stderr**, keeping **stdout** exclusively for the protocol.

The protocol is fully typed in `rho/src/rpc_wire.rs` — every method param, result, and notification has a Rust struct with `Serialize`/`Deserialize`. An [`OpenRPC 1.3.1` schema](../docs/rpc-schema/openrpc.json) is shipped in `docs/rpc-schema/` for client generation in any language. Run `cargo xtask schema` to update the version in the schema.

```
# Example session
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"fix the bug"},"id":1}' | rho --model <id>
```

The dispatch loop is decoupled from I/O via the [`Transport`] trait (`rho/src/transport.rs`). `StdioTransport` (newline-delimited JSON over stdin/stdout) is the default, but any `Transport` implementation — WebSocket, Unix socket, TCP — works without touching the dispatch logic. The public entry point (`run_rpc`) constructs a real `StdioTransport`; in-process tests inject a `StdioTransport` wired to canned readers and captured writers.

**Integration tests.** `rho/src/rpc.rs` ships a suite of end-to-end tests (70+ `#[tokio::test]` cases) covering the full JSON-RPC 2.0 protocol, including mid-turn steering and redirect approval flow. Tests use `MockChatClient` via `TestProvider` (in `rho-test-helpers`) and construct `App` directly (bypassing CLI startup). The concurrent reader architecture is tested via `ChannelTransport` (mpsc-backed) and `SyncTool` (a tool that blocks until released) to inject messages deterministically mid-turn.

### Protocol

**Methods (stdin → rho):**

| Method | Params | Description |
|---|---|---|
| `prompt` | `{message: string, steer?: boolean}` | Send a user message (or mid-turn steering nudge when `steer` is true) |
| `abort` | — | Cancel the current operation |
| `clear` | — | Clear conversation history |
| `getState` | — | Return model, provider, and cwd |
| `getMessages` | — | Return all messages on active path |
| `setModel` | `{model: string}` | Switch the active model |
| `listModels` | — | List available models from providers |
| `listProviders` | — | List configured providers with reachability |
| `getSessionStats` | — | Return token budget / usage info |
| `listSessions` | — | List previous sessions for project |
| `listExtensions` | — | List loaded extensions and tools |
| `reloadExtensions` | — | Reload extensions from disk |
| `compact` | — | Trigger context compaction |
| `resumeSession` | `{path: string}` | Resume a previous session |
| `listTools` | — | List registered tools with schemas and risk levels |
| `approvalResponse` | `{approved: boolean, message?: string}` | Respond to `approval/request`; `message` with `approved: false` is a redirect |

**Notifications (rho → stdout, no `id`):**

| Method | Params | Description |
|---|---|---|
| `ready` | — | Emitted once on startup |
| `agent/start` | — | Agent began processing a prompt |
| `agent/end` | `{reply, iterations, usage, toolCalls, durationMs, finishReason}` | Agent finished with full structured result |
| `agent/error` | `{error: string}` | Agent loop encountered an error |
| `state/change` | `{state: string}` | Loop state transition |
| `message/delta` | `{delta: string}` | Streaming text chunk |
| `reasoning/delta` | `{delta: string}` | Streaming reasoning chunk |
| `tool/call` | `{name, arguments}` | Model requested a tool call |
| `tool/result` | `{name, is_error, output}` | Tool finished executing |
| `tool/denied` | `{name}` | Tool call denied by approval gate |
| `approval/request` | `{tool, arguments, risk}` | Approval required — send `approvalResponse` |
| `usage` | `{iteration, usage, context}` | Per-iteration token/cost delta and live context snapshot |

**Error codes:**

| Code | Meaning |
|---|---|
| `-32700` | Parse error |
| `-32600` | Invalid request |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32603` | Internal error |

### Approval flow

When rho emits an `approval/request` notification it blocks until it reads an `approvalResponse` method from stdin:

```json
{"jsonrpc": "2.0", "method": "approvalResponse", "params": {"approved": true}, "id": 2}
```

Sending `approved: false` denies the tool call and lets the agent continue. When `approved: false` and `message` is set, the agent treats it as a **redirect**: the user's instructions are injected as a new turn and the model returns to thinking.

### Headless startup policy

- Provider consent requires `--accept-external-provider` (or `--endpoint`); no interactive prompt.
- Context file trust: already-trusted files load silently; new/changed files are auto-denied.
- Model resolution: resolves from config (`agent.model`, `agent.provider`, provider `default_model`) or CLI `--model`. No network calls at startup.

### `rho-test-helpers` — Shared Test Utilities (dev-only)

`MockChatClient`, `MockShellExecutor`, `TestProvider`, response builders, approval gates, file-system test environment, sandbox/trust helpers. `TestProvider` wraps a `MockChatClient` as a `Provider` impl, enabling integration tests that need a `ProviderRegistry` without a live model server.

## End-to-End Flow

`rho` operates as a continuous loop of **Reasoning → Action → Observation**, bridging an LLM and your local file system and shell.

### 1. Initialization & Configuration

Before the loop begins:

1. **Environment discovery** — Identify the project root (markers like `Cargo.toml`).
2. **Config loading** — Merge user-level (`~/.rho/config.toml`) and project-level (`.rho/config.toml`) configuration via `ConfigLoader`. Provider presets (`lm-studio`, `ollama`, `openrouter`, etc.) fill in endpoints automatically. Project-level providers merge with user-level by name. This selects the model, approval strictness, and command denylist.
3. **Tool registration** — Populate the `ToolRegistry` with built-in tools from `rho-tools`. Each tool provides a JSON schema (`ToolSchema`) so the model knows how to call it.
4. **Session construction** — Create or resume a `Session` (tree-shaped conversation with JSONL persistence).

### 2. The Agent Loop (`run_loop`)

The loop in `rho-core/src/agent.rs` returns `Result<AgentResult>`, capturing everything a consumer needs without implementing custom observers:

```rust
let result: AgentResult = rho_core::run_loop(&mut session, "fix the bug", &LoopParams {
    client: &service,
    registry: &tools,
    config: &agent_config,
    cancel: CancellationToken::new(),
    gate: &AutoApproveGate,
    observer: &NopObserver,
    compaction_client: None,
    steering: None,
}).await?;
```

A `CollectingObserver` is always active inside `run_loop` — it records every tool call into `AgentResult.tool_calls` automatically. Consumers don't need to build custom observers to get structured output.

The loop follows these steps:

#### A. Context Preparation

Before sending to the model, `rho` constructs a `ChatRequest` containing:
- **System prompt** — Instructions for API-driven agent behaviour.
- **Tool schemas** — Wire-format definitions of every available tool.
- **Conversation history** — Past `ChatMessage` values (System, User, Assistant, Tool).
- **Context management** — The `SlidingWindowContextManager` fits messages within the model's token budget, evicting old turns while pinning the system prompt.

#### B. Model Inference

The request is sent via the `LlmService` trait to an OpenAI-compatible API (e.g., LM Studio, Ollama). The model returns either:
1. **Text content** — A direct response (no more tools needed).
2. **Tool calls** — One or more `ModelToolCall` values requesting tool execution.

#### C. Approval & Execution

If the model requests tool calls:
1. **Risk assessment** — Each tool has a `ToolRisk` level (`Read`, `Write`, `Destructive`).
2. **Approval policy** — The `ApprovalPolicy` decides whether human confirmation is needed; if so, the `ApprovalGate` pauses and asks.
3. **Execution** — The `ToolRegistry` dispatches each call to the actual implementation (e.g., `ReadFile`, `RunCommand`).
4. **Safety layers** applied during execution:
   - **Sandbox check** — `SandboxRoot` validates every file path stays within the allowed directory.
   - **Redaction** — `Redactor` scans output for secrets (API keys, tokens) and replaces them with `[REDACTED]`.

#### D. Observation & Feedback

Tool results are wrapped in `Tool`-role `ChatMessage` values and appended to the conversation history. The model sees its previous action *and* the result, enabling it to reason about what to do next.

### 3. Iteration vs. Completion

- **Iteration** — The loop repeats. The model observes that `cargo test` failed, so it calls `EditFile` to fix the code, calls `CargoTest` again, and continues until all tests pass.
- **Completion** — The loop terminates when the model returns a text-only response with no pending tool calls (`LoopFinishReason::Stop`), or when a safety limit is reached (`MaxIterations`, `Cancelled`, `RetryBudgetExhausted`, `ConsecutiveEmptyResponses`). The finish reason is recorded in `AgentResult.finish_reason`.

~~~
Summary of data flow:

    User Input → Conversation History → LLM
         ↓ (if tool calls)
    Tool Call Request → Approval Gate → Tool Execution
         ↓ (sandbox check, redaction)
    Tool Result → appended to Conversation History → back to LLM
         ↓ (if text response)
    Text reply to user
~~~

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
| Stream timeouts | `first_token_timeout_secs`, `stream_idle_timeout_secs` | Convert hung LLM streams into retryable errors |
| Provider consent | `check_provider_consent()` | Warns before sending data to external servers |

## Project Layout

```
rho/                # Headless JSON-RPC 2.0 agent
  src/
    main.rs         # Thin entry: parse CLI, build App, run
    lib.rs          # Module declarations
    cli.rs          # `Cli` struct (model, endpoint, session flags, etc.)
    app.rs          # `App::build` (startup phases, extension loading) + `App::run` (JSON-RPC loop, `AgentResult` extraction)
    model.rs        # Model resolution
    ext_observer.rs # `CompositeObserver` — fans out to RPC observer + extension observers
    rpc.rs          # JSON-RPC 2.0 protocol: `run_rpc`, `run_rpc_on`, `RpcObserver`, `RpcApprovalGate`
    rpc_wire.rs      # Typed wire-format structs: params, results, notifications, `OpenRPC` schema types
    transport.rs       # `Transport` trait, `StdioTransport` (newline-delimited JSON)
    presenter.rs    # Presenter module root
    presenter/
      rpc.rs        # `RpcPresenter` — diagnostic output to stderr
rho-ai/             # Unified LLM provider abstraction
  src/
    lib.rs          # Re-exports: `LlmService`, `EventStream`, unified types
    service.rs      # `LlmService` trait
    openai.rs       # `OpenAiService` — OpenAI-compatible HTTP + SSE
    sse.rs          # Server-sent event parser
    retry.rs        # Exponential backoff retry logic
    catalog.rs      # `Catalog`, `Model`, `ModelCost` — built-in + user model registry
    catalog_generated.rs # Generated model list (`cargo xtask generate-models`)
    types.rs        # `LlmMessage`, `LlmRequest`, `StreamEvent`, `AccumulatedResponse`
    error.rs        # `ProviderError`
  model-overrides.json # Manual catalog overrides (thinking flags, compatibility)
rho-ext/            # TypeScript extension runtime
  src/
    lib.rs          # Re-exports: `ExtensionLoader`, `DenoTool`, `DenoObserver`, etc.
    runtime.rs      # `ExtensionRuntime` — V8 isolate on dedicated OS thread
    loader.rs       # `ExtensionLoader` — discovery, filtering, spawning, hot reload
    deno_tool.rs    # `DenoTool` — `Tool` trait wrapper
    deno_observer.rs# `DenoObserver` — `AgentObserver` trait wrapper
    manifest.rs     # `LoadedExtension` — manifest parsing and validation
    discover.rs     # Extension discovery (single-file and multi-file)
    transpile.rs    # TS → JS transpilation via `deno_ast`
    module_loader.rs# `RhoModuleLoader` — ESM module resolution for extensions
    host.rs         # `rho.*` host ops (log, readFile, writeFile, runCommand, getModel, getCwd)
    ops.rs          # Host-op helper macros (err!, json!, require_perm!, require_field!, js_fn!)
    async_dispatcher.rs # Shared background thread + tokio runtime for async ops
    host_shim.js    # ESM shim for extension module loading
    std_shim.js    # Standard library shim for extensions
    error.rs        # `ExtensionError` enum
  types/
    rho.d.ts        # TypeScript type definitions for extension authors
rho-core/           # Core library
  src/
    lib.rs          # Module declarations and convenience re-exports
    agent.rs        # Agent loop state machine, `run_loop`, `AgentResult`, `CollectingObserver`, `AgentObserver`
    approval.rs     # `ApprovalPolicy` and `ApprovalGate` traits
    client.rs       # `RhoAiClient`, `resolve_api_key`, `is_local_endpoint`, `ModelInfo`, `ModelList`
    client/         # Module directory
      error.rs      # `ClientError`
    config.rs       # `RhoConfig`, `ConfigLoader`, config sub-types
    context.rs      # `ContextManager` trait, `SlidingWindowContextManager`, `TokenBudget`
    context_files.rs# Project context file scanner, `TrustStore`
    conversation.rs # `Conversation`, `AssistantResponse`
    denylist.rs    # `CommandDenylist` — shell command denylist
    diagnostic.rs   # `Diagnostic`, `DiagnosticSeverity`, `DiagnosticSpan`
    error.rs        # `RhoError` and `Result`
    message.rs      # `ChatMessage`, `ContentBlock`, `ModelToolCall`
    model_match.rs  # `fuzzy_match`, `find_exact`, `format_suggestions`
    newtypes.rs     # `FilePath`, `ToolName`, `ToolCallId`, `DiagnosticCode`
    prompts/        # Prompt templates (loaded as `base_prompt()` / `compact_prompt()`)
      base.md       # Full agent system prompt
      compact.md    # Minimal prompt for small-context models
    provider.rs     # `Provider` trait, `OpenAiCompatibleProvider`, `ProviderRegistry`
    redact.rs       # `Redactor` — secret pattern matching
    request.rs      # `ChatRequest`
    response.rs     # `ModelResponse`, `FinishReason`
    sandbox.rs      # `SandboxRoot` — file sandbox validation
    schema.rs       # `ToolSchema` — wire-format tool definitions
    session.rs      # `Session` — tree-shaped conversation with JSONL persistence
    session/
      accessors.rs  # Read-side accessors (header, leaf, entry, model, system prompt)
      append.rs     # Write-side: append user/tool/compaction/branch/custom entries
      builder.rs    # `Session::new`, `open`, `in_memory`, config wiring
      compaction.rs # `CompactionStrategy`, `MechanicalCompactionStrategy`
      context.rs    # Context preparation: outlining, summarization, path messages
      context_stats.rs # `ContextStats` — context-window usage snapshot
      entry.rs      # `Entry`, `EntryPayload`, `EntryResolution`
      error.rs      # `SessionError`
      estimator.rs  # `TokenEstimator`, `HeuristicEstimator`
      eviction.rs   # Turn-internal eviction planner
      extensions.rs # `ExtensionEntry`, `ExtensionMessageEntry` traits
      header.rs     # `SessionHeader` — identity, version, origin metadata
      outliner.rs   # Entry outlining/summarization (per-tool structural summaries)
      persist.rs    # JSONL persistence, `SessionMetadata`
      phase.rs      # Session phase detection (agent work phase tracking)
      tree.rs       # Tree traversal: `path_to_root`, `branch_to`, `branch_with_summary`
      truncation.rs # Output truncation utilities
    shell.rs        # `ShellExecutor` trait, `ShellOutput`
    stream.rs       # Streaming helpers for the agent loop
    tool.rs         # `Tool` trait, `ToolRegistry`, `ToolResult`, `ToolRisk`
rho-tools/          # Built-in tool implementations
  src/
    lib.rs          # `register_all()`
    files.rs        # Re-exports file tools from `file_ops` + `edit`
    file_ops.rs     # `ReadFile`, `WriteFile`, `BatchRead`, `ListDir`
    edit.rs         # `EditFile` — hashline-anchored editing
    hashline.rs     # Hashline content-addressed editing
    shell.rs        # `PowerShellExecutor`, `RunCommand`, `CommandDenylist`
    crates_io.rs    # `CratesIoLookup` — crates.io metadata lookup
    session_summary.rs # `SessionSummary` — compressed turn history for recovery
    memory.rs       # `MemoryTool` — knowledge base (via `rho-memory`)
    rust/           # Rust tooling
      tools.rs      # `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
      rustdoc.rs    # `RustdocTool` — stdlib docs from local rustdoc
      convert.rs    # Raw cargo JSON → core diagnostic conversion
      format.rs     # Diagnostic formatting and AST context
      parse.rs      # NDJSON parsing for `--message-format=json`
      types.rs      # Raw cargo JSON types (module-private)
rho-memory/         # Persistent knowledge base (SQLite/FTS5)
  src/
    lib.rs          # Re-exports: `Memory`, `Document`, `SearchResult`, `Error`
    brain.rs        # `Memory` — public knowledge-base API
    db.rs           # `Database` — raw SQLite operations
    models.rs       # `Document`, `SearchResult`, `CreateRequest`, `UpdateRequest`, `Stats`
    error.rs        # `Error`
  migrations/
    001_initial.sql # Schema: documents, FTS5, triggers
rho-highlight/      # Tree-sitter syntax analysis
  src/
    lib.rs          # Re-exports
    lang.rs         # `Language` enum — grammar selection (feature-gated)
    parse.rs        # Tree-sitter parsing
    highlight.rs    # Token classification and highlighting
    query.rs        # AST node lookup by position
    error.rs        # `HighlightError`
rho-test-helpers/   # Shared test infrastructure (dev-only)
  src/
    lib.rs          # `MockChatClient`, response builders, helpers
xtask/              # Dev task runner
  src/
    main.rs         # CLI dispatch
    tasks.rs        # `ci`, `test`, `build`, `release`, `changelog`, `fmt`, `fmt-fix`, `lint`, `run`, `clean`, `status`, `schema`, `generate-models` tasks
Cargo.toml          # Workspace root
CHANGELOG.md        # Generated via git-cliff
cliff.toml          # git-cliff configuration
docs/rpc-schema/openrpc.json  # OpenRPC 1.3.1 schema (machine-readable API spec)
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

### Agent loop output

| Type | Location | Purpose |
|---|---|---|
| `AgentResult` | `agent.rs` | Structured output from `run_loop`: reply, iterations, usage, tool calls, duration, finish reason, context stats |
| `TokenUsage` | `agent.rs` | Per-call token delta (input, output, cost, request count) |
| `ToolCallRecord` | `agent.rs` | Single tool call record (name, arguments, outcome, duration) |
| `ToolCallOutcome` | `agent.rs` | What happened to a tool call (`Success`, `Error`, `Denied`, `Blocked`) |
| `LoopFinishReason` | `agent.rs` | Why `run_loop` terminated (`Stop`, `MaxIterations`, `Cancelled`, `RetryBudgetExhausted`, `ConsecutiveEmptyResponses`) |
| `CollectingObserver` | `agent.rs` | Always-active observer that records tool call events into `ToolCallRecord`s |
| `SteeringSource` | `agent.rs` | Trait for mid-turn steering message drain (sync, dyn-safe) |
| `SteeringQueue` | `agent.rs` | Thread-safe `SteeringSource` impl (Arc-shared VecDeque, push/drain) |
| `LoopParams` | `agent.rs` | Parameter bundle for `run_loop` (client, registry, config, cancel, gate, observer, compaction_client, steering) |

### Tools

| Type | Location | Purpose |
|---|---|---|
| `Tool` | `tool.rs` | Trait: `name`, `description`, `schema`, `risk`, `execute` |
| `ToolRegistry` | `tool.rs` | Maps tool names to `Box<dyn Tool>` |
| `ToolRisk` | `tool.rs` | `Read`, `Write`, `Destructive`, `Network` |
| `ToolResult` | `tool.rs` | Result of a tool execution |
| `ToolSchema` | `rho-core/src/schema.rs` | Wire-format tool definition |
| `CancellationToken` | `tool.rs` | Cooperative cancellation signal |

### Client and provider

| Type | Location | Purpose |
|---|---|---|
| `LlmService` | `rho-ai/service.rs` | Trait: `chat_stream(LlmRequest) → EventStream` |
| `LlmRequest` | `rho-ai/types.rs` | Request: model, messages, tools, max_tokens, reasoning_effort |
| `OpenAiService` | `rho-ai/openai.rs` | OpenAI-compatible HTTP client with SSE streaming |
| `StreamEvent` | `rho-ai/types.rs` | Streaming response event (`Text`, `Reasoning`, `ToolUse*`, `Done`) |
| `AccumulatedResponse` | `rho-ai/types.rs` | Fully-accumulated response (text + tool calls + usage) |
| `Catalog` | `rho-ai/catalog.rs` | Built-in + user model registry (find, search, by_provider) |
| `Model` | `rho-ai/catalog.rs` | Catalog entry: context window, thinking support, pricing |
| `ModelCost` | `rho-ai/catalog.rs` | Per-million-token pricing (input, output, cache-read, cache-write) |
| `RhoAiClient` | `client.rs` | Wraps `LlmService` for use in the agent loop |
| `Provider` | `provider.rs` | Trait: `name`, `is_external`, `list_models`, `clone_boxed_service`, `llm_service` |
| `ProviderRegistry` | `provider.rs` | Ordered collection of `Box<dyn Provider>` |
| `ModelInfo` | `client.rs` | A model entry from `/v1/models` (accepts both `id` and `slug` fields via untagged enum) |
| `ModelList` | `client.rs` | Model list response (accepts both `data` array and `models` array via untagged enum) |
| `ProviderConfig` | `config.rs` | Provider config: name, preset, endpoint, API key env var, default_model, models_endpoint |
| `rho_ai::ProviderConfig` | `rho-ai/types.rs` | API key and base URL (model is per-request, not per-provider) |

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
| `AgentConfig` | `agent.rs` | Loop settings: iterations, retry, backoff, compaction mode |
| `MemoryConfig` | `config.rs` | `[memory]` section: `enabled` toggle for project-local knowledge base |

### Errors

| Type | Location | Purpose |
|---|---|---|
| `RhoError` | `error.rs` | Thin boundary enum wrapping domain-specific errors (Agent, Client, Session, Sandbox, plus cross-domain variants) |
| `ClientError` | `client/error.rs` | HTTP, JSON parsing, retry budget exhaustion |
| `AgentError` | `agent.rs` | Agent loop: max iterations, cancellation, protocol violations |
| `SessionError` | `session/error.rs` | Session management: entry not found, persistence |
| `SandboxError` | `sandbox.rs` | File sandbox: path validation, security violations |
| `ToolError` | `rho-tools/src/error.rs` | Tool operations: file access, command execution, sandbox violations |
| `HighlightError` | `rho-highlight/src/error.rs` | Syntax highlighting: grammar not available, parse failure, position errors |
| `Error` | `rho-memory/src/error.rs` | Knowledge base: database, JSON, timestamp, I/O, input errors |
| `Result<T>` | `error.rs` | `std::result::Result<T, RhoError>` |

### Newtypes

| Type | Location | Purpose |
|---|---|---|
| `FilePath` | `newtypes.rs` | Sandboxed file paths |
| `ToolName` | `newtypes.rs` | Tool names |
| `ToolCallId` | `newtypes.rs` | Model-issued tool call IDs |
| `DiagnosticCode` | `newtypes.rs` | Rust compiler diagnostic codes |

### Extensions



| Type | Location | Purpose |
|---|---|---|
| `ExtensionRuntime` | `rho-ext/src/runtime.rs` | V8 isolate on dedicated OS thread |
| `ExtensionLoader` | `rho-ext/src/loader.rs` | Discovery, filtering, spawning, hot reload |
| `DenoTool` | `rho-ext/src/deno_tool.rs` | `Tool` trait wrapper for extension functions |
| `DenoObserver` | `rho-ext/src/deno_observer.rs` | `AgentObserver` wrapper for extension hooks |
| `LoadedExtension` | `rho-ext/src/manifest.rs` | Parsed extension manifest (tools, hooks, commands) |
| `CompositeObserver` | `rho/src/ext_observer.rs` | Fans out to RPC observer + extension observers |
| `ExtensionError` | `rho-ext/src/error.rs` | Transpile, load, manifest, execution errors |
| `InterceptResult` | `rho-core/src/agent.rs` | `Block`/`Allow` for tool-call interception

## Platform Support

Windows, macOS, Linux. PowerShell 7+ (`pwsh`) is the primary shell on all platforms; Windows PowerShell 5.1 (`powershell`) is the fallback on Windows only. Path normalization is platform-aware.