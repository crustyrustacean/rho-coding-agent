# Crate Responsibilities

## `rho` (binary)

The REPL loop, CLI argument parsing, tool wiring, and system prompt assembly. Constructs a `Session`, connects to the model via `LocalChatClient`, and drives the agent loop. Handles CLI dispatch (REPL mode, prompt-file mode, session resume, ephemeral mode).

Key responsibilities: provider consent check, model resolution, startup budget diagnostics, shell-specific prompt extensions.

## `rho-core`

The kernel: agent loop state machine, session management (tree persistence, compaction, branching), context window management (sliding window, token estimation), tool registry and traits, approval gates, secret redaction, file sandbox, model client trait, configuration loading, and all shared types.

Key types: `Session`, `AgentConfig`, `ToolRegistry`, `Tool`, `ChatClient`, `ContextManager`, `TokenBudget`, `SandboxRoot`, `Redactor`, `RhoConfig`.

## `rho-highlight`

Tree-sitter-based syntax analysis. Provides `parse()`, `highlight()`, and `node_at()` for Rust source code. Used by `EditFile` for node-splitting validation and by the TUI (Phase 4) for syntax highlighting.

Key types: `Language`, `Highlight`, `NodeInfo`.

## `rho-tools`

Built-in tool implementations:

- **File tools:** `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
- **Shell tools:** `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
- **Rust tools:** `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`

Each implements the `Tool` trait from `rho-core`.

## `rho-test-helpers`

Shared test infrastructure (dev-only): `MockChatClient`, `MockShellExecutor`, `AutoApproveGate`, `AutoDenyGate`, `FixedResponseTool`, `FileTestEnv`, `in_memory_session`, `empty_trust_store`, `detect_shell`, `tempdir_with_sandbox`, `assert_no_orphan_tool_results`. Never published.

## `rho-eval`

Behavioural benchmark suite (dev-only): canonical coding tasks, automated scoring, prompt SHA-256 tracking, regression gating. Used to validate end-to-end agent behaviour across model releases.

## `xtask`

Dev task runner: `cargo xtask ci` (fmt → lint → build → test), `cargo xtask test`, `cargo xtask changelog`.
