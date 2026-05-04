# Crate Responsibilities

## `rho` (binary)

The REPL loop, CLI argument parsing, and tool wiring. Constructs a `Session`, connects to the model via `LocalChatClient`, and drives the agent loop.

## `rho-core`

The kernel: agent loop, session management, context building, tool registry, approval gates, secret redaction, file sandbox, and model client trait. Every other crate depends on this.

Key types: `Session`, `AgentConfig`, `ToolRegistry`, `ChatClient`, `ContextManager`, `SandboxRoot`, `Redactor`.

## `rho-tools`

Built-in tool implementations: `ReadFile`, `WriteFile`, `ListDir`, `EditFile`, `RunCommand`. Each implements the `Tool` trait from `rho-core`.

## `rho-test-helpers`

Shared test infrastructure (dev-only): `MockChatClient`, `MockShellExecutor`, `AutoApproveGate`, `FileTestEnv`, `in_memory_session`. Never published.
