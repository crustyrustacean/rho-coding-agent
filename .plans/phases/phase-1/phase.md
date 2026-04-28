# Phase 1: The Agent Loop

**Goal:** The model can invoke tools and receive structured results. The agent runs autonomously until it's done or needs user input.

**Milestone:** `rho` reads a file when asked, instead of just saying "I would read the file."

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `tree-sitter` **(foundation)** | `rho-highlight` | Standard parser generator runtime — writing a parser from scratch is not feasible |
| `tree-sitter-rust` **(foundation)** | `rho-highlight` | Rust grammar — this *is* the spec, thousands of rules, not writable by hand |

No other new dependencies. Tool trait, tool registry, agent loop, `ChatClient` trait, and the three basic tools are all straightforward Rust that we write ourselves.

## Exit Criteria

The agent can read, write, and execute commands when the model requests it. The loop terminates correctly on `Stop` and handles retryable errors with backoff. `rho-highlight` can parse and highlight a Rust source file. `AssistantResponse` carries `Vec<ModelToolCall>` (loop acts on first only). `rho-test-helpers` provides a reusable mock `ChatClient`. `ContextManager` slides the window (pinning the system message) before API errors occur. Command-surface APIs exist on `Conversation` and `ToolRegistry` (even though only `/quit` and `/clear` are wired up in the REPL). Project context files (`AGENTS.md`, `.cursorrules`, etc.) are detected and loaded into the system prompt on startup.

**Security:** destructive tools require approval even in the bare REPL, file tools enforce the sandbox root, `Role::Context` separates file contents from user instructions, secret patterns are redacted from tool results, and project context files require trust confirmation on first load.
