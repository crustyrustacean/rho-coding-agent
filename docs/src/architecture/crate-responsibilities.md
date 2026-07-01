# Crate Responsibilities

## `rho` (binary)

The headless JSON-RPC 2.0 agent: CLI argument parsing, tool wiring, extension loading, and system prompt assembly. Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes responses/notifications to stdout. Diagnostic output goes to stderr.

Constructs a `Session`, connects to the model via a `Provider`, and drives the agent loop. Handles provider consent checks, model resolution, startup budget diagnostics, shell-specific prompt extensions, session discovery, extension loading, and live observer output via `RpcObserver`.

The RPC implementation is decoupled from I/O via the `Transport` trait (`rho/src/transport.rs`); `StdioTransport` (newline-delimited JSON over stdin/stdout) is the default, and in-process tests inject a `StdioTransport` wired to canned readers and captured writers. Wire-format types in `rpc_wire.rs` give every method param, result, and notification a typed Rust struct. See [RPC Mode](../rpc-mode.md) for the full protocol reference and [`OpenRPC` schema](../../rpc-schema/openrpc.json) for machine-readable API discovery.

## `rho-core`

The kernel: agent loop state machine (with `AgentObserver` for live output, `CollectingObserver` for structured tool call recording, phase tracking, auto-compaction) returning `AgentResult` with full structured output (reply, iterations, token usage, tool call history, duration, finish reason, context stats), session management (tree persistence, graduated resolution, phase-aware compaction, LLM compaction, selective eviction, branching, session discovery), context window management (sliding window, token estimation, enhanced `ContextStats` with role/resolution/phase token distributions), tool registry and traits, approval gates, secret redaction, file sandbox, provider abstraction (`Provider` trait, `ProviderRegistry` with named presets and additive config merge), LLM client (`RhoAiClient`), configuration loading (two-tier TOML with provider preset resolution), and all shared types.

Key types: `Session`, `AgentConfig`, `AgentObserver`, `NopObserver`, `CollectingObserver`, `AgentResult`, `TokenUsage`, `ToolCallRecord`, `ToolCallOutcome`, `LoopFinishReason`, `LoopParams`, `ContextStats`, `RoleTokenDistribution`, `ResolutionTokenDistribution`, `PhaseTokenDistribution`, `SessionMetadata`, `ToolRegistry`, `Tool`, `Provider`, `ProviderRegistry`, `ContextManager`, `TokenBudget`, `SandboxRoot`, `Redactor`, `RhoConfig`, `SessionPhase`, `CompactionPhase`, `CompactionStrategy`, `LlmCompactionStrategy`, `MechanicalCompactionStrategy`, `CommandDenylist`.


## `rho-highlight`

Tree-sitter-based syntax analysis. Provides `parse()`, `highlight()`, and `node_at()` for Rust source code. Used by `EditFile` for node-splitting validation and by tools that need structural code awareness.

Key types: `Language`, `HighlightSpan`, `HighlightTag`, `NodeInfo`.


## `rho-tools`

Built-in tool implementations:

- **File tools:** `ReadFile`, `BatchRead`, `WriteFile`, `ListDir`, `EditFile`
- **Shell tools:** `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
- **Rust tools:** `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
- **Lookup tools:** `RustdocTool`, `CratesIoLookup`
- **Session tools:** `SessionSummary` (compressed turn history for context recovery)
- **Knowledge base:** `MemoryTool` (project-local docs via `rho-memory`, gated by `[memory] enabled`)

Each implements the `Tool` trait from `rho-core`.

## `rho-memory`

Persistent knowledge base for project-local docs: full-text search (SQLite FTS5), content deduplication, and soft deletes. Exposed to the agent through the `MemoryTool` in `rho-tools`. Storage lives at `<project-root>/.rho/memory.db`. Gated by the `[memory]` config section (`enabled = true`).

Key types: `Memory`, `Database`, `Document`, `SearchResult`.

## `rho-ext`

TypeScript extension runtime (powered by `deno_core` and V8). Enables user-authored extensions that add tools, hooks, and commands to rho.

- **`ExtensionRuntime`** — owns a V8 isolate on a dedicated OS thread, handles async tool/hook/command calls via tokio channels
- **`ExtensionLoader`** — orchestrates discovery (single-file and multi-file), config-driven filtering, spawning, tool registration, and hot reload (mtime-based change detection)
- **`DenoTool`** — wraps extension tool functions as `Box<dyn Tool>` for the `ToolRegistry`
- **`DenoObserver`** — wraps extension hooks as `AgentObserver` for the agent loop (onLoad, onToolCall, onToolResult, onBeforeModel)
- **TypeScript transpilation** — `deno_ast` transpiles `.ts` to JS at load time
- **Host functions** — `rho.log()`, `rho.readFile()`, `rho.writeFile()`, `rho.runCommand()`, `rho.getModel()`, `rho.getCwd()` with permission gating
- **Type definitions** — ships `rho.d.ts` for extension author IntelliSense

Key types: `ExtensionRuntime`, `ExtensionLoader`, `DenoTool`, `DenoObserver`, `LoadedExtension`, `ExtensionError`.

See [Extensions](../extensions.md) for the full extension system documentation.

## `rho-test-helpers`

Shared test infrastructure (dev-only): `MockChatClient`, `TestProvider`, `MockShellExecutor`, `AutoApproveGate`, `AutoDenyGate`, `FixedResponseTool`, `FailingTool`, `FileTestEnv`, `in_memory_session`, `empty_trust_store`, `detect_shell`, `tempdir_with_sandbox`, `assert_no_orphan_tool_results`. Depends on `rho-core` and `rho-ai`. Never published.

`TestProvider` wraps a `MockChatClient` as a `Provider` impl, enabling integration tests that need a `ProviderRegistry` without a live model server.

## `xtask`

Dev task runner: `cargo xtask ci` (fmt → lint → build → test), `cargo xtask test`, `cargo xtask build`, `cargo xtask release`, `cargo xtask changelog`, `cargo xtask fmt`, `cargo xtask fmt-fix`, `cargo xtask run`, `cargo xtask clean`, `cargo xtask status`, `cargo xtask schema`, `cargo xtask generate-models`. Tests use `cargo-nextest` when available, falling back to `cargo test`.