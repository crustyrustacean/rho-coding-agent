# Crate Responsibilities

## `rho` (binary)

The binary entry point: CLI argument parsing, tool wiring, and system prompt assembly. Supports two execution modes:

- **REPL mode** (default) — interactive terminal session with slash commands and live output
- **RPC mode** (`--mode rpc`) — headless JSONL over stdin/stdout for process integration

Constructs a `Session`, connects to the model via a `Provider`, and drives the agent loop. Handles provider consent checks, model resolution, startup budget diagnostics, shell-specific prompt extensions, session discovery (`rho -c`), and live observer output (reasoning deltas, tool activity).

The RPC implementation is generic over I/O (`run_rpc_on<R, W>`) so the 43 in-process integration tests can inject canned stdin and capture stdout without touching real file descriptors. See [RPC Mode](../rpc-mode.md) for the full protocol reference.

## `rho-core`

The kernel: agent loop state machine (with `AgentObserver` for live output), session management (tree persistence, compaction, branching, session discovery), context window management (sliding window, token estimation, `ContextStats`), tool registry and traits, approval gates, secret redaction, file sandbox, provider abstraction (`Provider` trait, `ProviderRegistry`), LLM client (`RhoAiClient`), configuration loading, and all shared types.

Key types: `Session`, `AgentConfig`, `AgentObserver`, `NopObserver`, `ContextStats`, `SessionMetadata`, `ToolRegistry`, `Tool`, `Provider`, `ProviderRegistry`, `ContextManager`, `TokenBudget`, `SandboxRoot`, `Redactor`, `RhoConfig`.


## `rho-highlight`

Tree-sitter-based syntax analysis. Provides `parse()`, `highlight()`, and `node_at()` for Rust source code. Used by `EditFile` for node-splitting validation and by the TUI (Phase 4) for syntax highlighting.

Key types: `Language`, `Highlight`, `NodeInfo`.


## `rho-tools`

Built-in tool implementations:

- **File tools:** `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
- **Shell tools:** `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
- **Rust tools:** `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
- **Lookup tools:** `RustdocTool`, `CratesIoLookup`

Each implements the `Tool` trait from `rho-core`.

## `rho-test-helpers`

Shared test infrastructure (dev-only): `MockChatClient`, `TestProvider`, `MockShellExecutor`, `AutoApproveGate`, `AutoDenyGate`, `FixedResponseTool`, `FailingTool`, `FileTestEnv`, `in_memory_session`, `empty_trust_store`, `detect_shell`, `tempdir_with_sandbox`, `assert_no_orphan_tool_results`. Never published.

`TestProvider` wraps a `MockChatClient` as a `Provider` impl, enabling integration tests that need a `ProviderRegistry` without a live model server.

## `rho-eval`

Behavioural benchmark suite (dev-only): canonical coding tasks via the `EvalTask` trait, automated pass/fail scoring with `TaskOutcome`, performance metrics (`TaskMetrics`: wall time, token usage, agent iterations), prompt SHA-256 tracking, and regression gating. Used by `rho-bench` to drive end-to-end evaluation against real models.

## `rho-bench`

Benchmark harness binary (dev-only): runs `rho-eval` tasks against one or more local models and produces structured comparison reports. Creates isolated temp Cargo projects per task, drives `run_loop` with a `CountingClient` wrapper for token tracking, and persists results as JSON. Supports multi-model sweeps, repeat runs for statistical reliability, compact prompts for small-context models, and both terminal table and JSON output formats.

## `xtask`

Dev task runner: `cargo xtask ci` (fmt → lint → build → test), `cargo xtask test`, `cargo xtask changelog`.