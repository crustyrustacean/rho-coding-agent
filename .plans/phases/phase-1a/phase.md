# Phase 1a: The Agent Loop

**Goal:** The model can invoke tools and receive structured results. The agent runs autonomously until it's done or needs user input.

**Milestone:** `rho` reads a file when asked, instead of just saying "I would read the file."

## Scope

Phase 1 was originally a single phase. It has been split because the original scope packed two distinct deliverables into one: the agent-loop machinery, and the security surface (sandbox, approval policy, context-file trust, redaction). Each deserves its own focused pass with its own tests.

Phase 1a covers the agent-loop machinery. The bare REPL produced at the end of Phase 1a is **not yet safe** — destructive tool calls execute without approval and file paths are not sandboxed. That gap is closed by Phase 1b before the agent is exposed to anything resembling daily use.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `async-trait` **(foundation)** | `rho-core` | Required for `Box<dyn Tool>` and `Box<dyn ChatClient>`. Native async-fn-in-traits (stabilised in Rust 1.75) is not dyn-compatible — see [Rust reference](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility). The agent's tool registry and provider swap (`/provider`) both require dynamic dispatch. `trait-variant` solves the `Send` bound problem but does not provide dyn compatibility. The hand-rolled alternative (returning `Pin<Box<dyn Future + Send>>` everywhere) is what `async-trait` already does, just without the macro — accept the macro |

No other new dependencies in Phase 1a. Tool trait, tool registry, agent loop, `ChatClient` trait, `ContextManager`, and the three tool stubs are all straightforward Rust we write ourselves. `tree-sitter` is **deferred to Phase 3**, where its first real consumer (mapping diagnostic spans to AST nodes) appears.

## Exit Criteria

The agent can read, write, and execute commands when the model requests it. The loop terminates correctly on `Stop` and handles retryable errors with backoff. `AssistantResponse` carries `Vec<ModelToolCall>` (loop acts on first only in Phase 1a; multi-call handling is Phase 2). Assistant messages with `tool_calls` are persisted into history *before* their tool-result messages are appended (otherwise the next API request is rejected). `rho-test-helpers` provides a reusable mock `ChatClient`. `ContextManager` is a trait with a sliding-window default that pins the system message and evicts by *turn*, never splitting an assistant `tool_calls` message from its matching `tool` results. Command-surface APIs exist on `Conversation` and `ToolRegistry` (only `/quit` and `/clear` are wired up in the REPL). The bare REPL is functional but explicitly not yet hardened — Phase 1b adds the security surface.

## Non-Goals (deferred to Phase 1b)

- Approval policy and prompts
- File sandbox and `FilePath` validation
- `Role::Context` (or its successor — see Phase 1b for the shape question)
- Project context file scanning and trust
- Secret redaction
