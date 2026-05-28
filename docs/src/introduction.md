# Introduction

**rho** is a Rust coding agent that runs against local and remote LLMs.

It connects to OpenAI-compatible endpoints (LM Studio, Ollama, OpenAI, Groq, OpenRouter, DeepInfra, and more), gives the model access to tools for reading and writing files, executing shell commands, and running Rust tooling — then runs an autonomous agent loop that the user supervises through an approval gate.

rho supports two execution modes:

- **REPL mode** (default) — interactive terminal session with slash commands and live output
- **RPC mode** (`--mode rpc`) — headless JSONL over stdin/stdout for embedding in editors, bots, and custom UIs

See [RPC Mode](./rpc-mode.md) for the full protocol reference.

Design priorities:

- **Local first** — your code stays on your machine by default
- **Remote ready** — external providers (OpenAI, Groq, etc.) with consent warnings
- **Safe by default** — destructive actions require your approval
- **Rust-native** — structured compiler diagnostics, tree-sitter syntax analysis, not text scraping
- **Extensible** — custom tools and hooks via TypeScript extensions (V8/deno-core), hot-reloadable at runtime

## Platform support

rho runs on Windows, macOS, and Linux. PowerShell 7+ (`pwsh`) is the primary shell on all platforms; Windows PowerShell 5.1 (`powershell`) is the fallback on Windows only.

## What can rho do?

- Read, write, and edit files within a sandboxed project directory
- Execute shell commands with a safety denylist
- Run `cargo check`, `cargo clippy`, `cargo test`, `cargo fix`, and `rustc --explain` with structured output
- Detect syntax node splits during edits via tree-sitter
- Manage conversation state across sessions with tree-structured persistence and session discovery
- Observe agent activity in real time (reasoning, tool calls, errors) via the `AgentObserver` trait
- Extend rho with TypeScript extensions that add tools, hooks, and commands — hot-reloadable at runtime
- Compact old conversation turns to stay within context limits
- Monitor context window usage with a live status bar and `/status` REPL command
- Run headless via JSONL-over-stdio RPC mode for integration with editors, bots, and custom UIs

This book documents the architecture, security model, configuration, and development of rho.
