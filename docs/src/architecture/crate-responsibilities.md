# Crate Responsibilities

## `rho` (binary)

The headless JSON-RPC 2.0 agent: CLI argument parsing, tool wiring, extension loading, and system prompt assembly. Reads newline-delimited JSON-RPC 2.0 requests from stdin and writes responses/notifications to stdout. Diagnostic output goes to stderr.

Constructs a `Session`, connects to the model via a `Provider`, and drives the agent loop. Handles provider consent checks, model resolution, startup budget diagnostics, shell-specific prompt extensions, session discovery, extension loading, and live observer output via `RpcObserver`.

The RPC implementation is generic over I/O (`run_rpc_on<R, W>`) so the in-process integration tests can inject canned stdin and capture stdout without touching real file descriptors. See [RPC Mode](../rpc-mode.md) for the full protocol reference.

## `rho-core`

The kernel: agent loop state machine (with `AgentObserver` for live output, phase tracking, auto-compaction), session management (tree persistence, graduated resolution, phase-aware compaction, LLM compaction, selective eviction, branching, session discovery), context window management (sliding window, token estimation, enhanced `ContextStats` with role/resolution/phase token distributions), tool registry and traits, approval gates, secret redaction, file sandbox, provider abstraction (`Provider` trait, `ProviderRegistry`), LLM client (`RhoAiClient`), configuration loading, and all shared types.

Key types: `Session`, `AgentConfig`, `AgentObserver`, `NopObserver`, `ContextStats`, `RoleTokenDistribution`, `ResolutionTokenDistribution`, `PhaseTokenDistribution`, `SessionMetadata`, `ToolRegistry`, `Tool`, `Provider`, `ProviderRegistry`, `ContextManager`, `TokenBudget`, `SandboxRoot`, `Redactor`, `RhoConfig`, `SessionPhase`, `CompactionPhase`, `CompactionStrategy`, `LlmCompactionStrategy`, `MechanicalCompactionStrategy`, `CommandDenylist`.


## `rho-repl`

Interactive terminal client that spawns `rho` as a subprocess and communicates via JSON-RPC 2.0 over stdin/stdout. Provides readline input with persistent history (`~/.rho/repl-history`), slash command dispatch, streaming output rendering, and approval prompts.

Does **not** depend on any rho crate — all interaction is through the JSON-RPC 2.0 protocol. Uses `rustyline` for input, `colored` for terminal colors, and `tokio` for async subprocess I/O.

Key types: `Cli`, `RhoClient`, `Renderer`.


## `rho-highlight`

Tree-sitter-based syntax analysis. Provides `parse()`, `highlight()`, and `node_at()` for Rust source code. Used by `EditFile` for node-splitting validation and by tools that need structural code awareness.

Key types: `Language`, `Highlight`, `NodeInfo`.


## `rho-tools`

Built-in tool implementations:

- **File tools:** `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
- **Shell tools:** `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
- **Rust tools:** `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
- **Lookup tools:** `RustdocTool`, `CratesIoLookup`

Each implements the `Tool` trait from `rho-core`.

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

## `rho-eval`

Behavioural benchmark suite (dev-only): canonical coding tasks via the `EvalTask` trait, automated pass/fail scoring with `TaskOutcome`, performance metrics (`TaskMetrics`: wall time, token usage, agent iterations), prompt SHA-256 tracking, and regression gating. Used by `rho-bench` to drive end-to-end evaluation against real models.

## `rho-bench`

Benchmark harness binary (dev-only): runs `rho-eval` tasks against one or more local models and produces structured comparison reports. Creates isolated temp Cargo projects per task, drives `run_loop` with a `CountingClient` wrapper for token tracking, and persists results as JSON. Supports multi-model sweeps, repeat runs for statistical reliability, compact prompts for small-context models, and both terminal table and JSON output formats.

## `xtask`

Dev task runner: `cargo xtask ci` (fmt → lint → build → test), `cargo xtask test`, `cargo xtask build`, `cargo xtask release`, `cargo xtask changelog`, `cargo xtask fmt`, `cargo xtask run`, `cargo xtask clean`, `cargo xtask status`. Tests use `cargo-nextest` when available, falling back to `cargo test`.