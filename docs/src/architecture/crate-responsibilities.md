# Crate Responsibilities

## `rho` (binary)

The REPL loop, CLI argument parsing, tool wiring, and system prompt assembly. Constructs a `Session`, connects to the model via `LocalChatClient`, and drives the agent loop. Handles CLI dispatch (REPL mode, prompt-file mode, session resume, ephemeral mode).

Key responsibilities: provider consent check, model resolution, startup budget diagnostics, shell-specific prompt extensions, session discovery (`rho -c`), and live observer output (reasoning deltas, tool activity).

## `rho-core`

The kernel: agent loop state machine (with `AgentObserver` for live output), session management (tree persistence, compaction, branching, session discovery), context window management (sliding window, token estimation, `ContextStats`), tool registry and traits, approval gates, secret redaction, file sandbox, model client trait, configuration loading, and all shared types.

Key types: `Session`, `AgentConfig`, `AgentObserver`, `NopObserver`, `ContextStats`, `SessionMetadata`, `ToolRegistry`, `Tool`, `ChatClient`, `ContextManager`, `TokenBudget`, `SandboxRoot`, `Redactor`, `RhoConfig`.


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

Shared test infrastructure (dev-only): `MockChatClient`, `MockShellExecutor`, `AutoApproveGate`, `AutoDenyGate`, `FixedResponseTool`, `FileTestEnv`, `in_memory_session`, `empty_trust_store`, `detect_shell`, `tempdir_with_sandbox`, `assert_no_orphan_tool_results`. Never published.

## `rho-eval`

Behavioural benchmark suite (dev-only): canonical coding tasks via the `EvalTask` trait, automated pass/fail scoring with `TaskOutcome`, performance metrics (`TaskMetrics`: wall time, token usage, agent iterations), prompt SHA-256 tracking, and regression gating. Used by `rho-bench` to drive end-to-end evaluation against real models.

## `rho-bench`

Benchmark harness binary (dev-only): runs `rho-eval` tasks against one or more local models and produces structured comparison reports. Creates isolated temp Cargo projects per task, drives `run_loop` with a `CountingClient` wrapper for token tracking, and persists results as JSON. Supports multi-model sweeps, repeat runs for statistical reliability, compact prompts for small-context models, and both terminal table and JSON output formats.

## `xtask`

Dev task runner: `cargo xtask ci` (fmt → lint → build → test), `cargo xtask test`, `cargo xtask changelog`.