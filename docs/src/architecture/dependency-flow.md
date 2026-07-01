# Dependency Flow

Dependencies flow downward only. A crate may depend on crates below it in the stack but never above.

```
rho (binary) ──────────────────────────────────────────
    │
    ├── rho-ext ──── rho-core ──── rho-ai ──────────────
    │
    ├── rho-tools ── rho-highlight ── rho-core ─────────
    │           └──── rho-memory ── rho-core ────────────
    │
    └── rho-core ──── rho-ai ──────────────────────────

rho-test-helpers ── rho-core, rho-ai ────────────────
xtask ──────────────────────────────────────────────
```

| Crate | Depends on | Notes |
|---|---|---|
| `rho` | `rho-core`, `rho-tools`, `rho-ext`, `rho-ai` | Headless JSON-RPC 2.0 agent |
| `rho-ext` | `rho-core`, `deno_core`, `deno_ast` | TypeScript extension runtime |
| `rho-tools` | `rho-core`, `rho-highlight`, `rho-memory` | Tool implementations; highlight for node-splitting, memory for `MemoryTool` |
| `rho-memory` | `rho-core` | Persistent knowledge base (SQLite/FTS5) |
| `rho-highlight` | `rho-core` | Tree-sitter grammar; uses core diagnostic types |
| `rho-core` | `rho-ai` | Kernel — the foundation everything else builds on |
| `rho-ai` | none (external only) | Unified LLM provider abstraction |
| `rho-test-helpers` | `rho-core`, `rho-ai` | Dev-only — provides mocks and fixtures for testing |
| `xtask` | none (cargo integration) | Dev-only — task runner, no rho crate dependencies |

## Key external dependencies

| Dependency | Used by | Purpose |
|---|---|---|
| `reqwest` | `rho-ai` | HTTP client for model API |
| `tokio` | `rho-core`, `rho-tools`, `rho-ext` | Async runtime |
| `serde` / `serde_json` | everywhere | Serialization |
| `toml` | `rho-core` | Configuration parsing |
| `tree-sitter` + grammars | `rho-highlight` | Syntax analysis |
| `ignore` | `rho-tools` | `.gitignore`-aware directory listing |
| `clap` | `rho` | CLI argument parsing |
| `tracing` | `rho-core`, `rho-ext` | Structured logging |
| `deno_core` | `rho-ext` | V8 JavaScript runtime |
| `deno_ast` | `rho-ext` | TypeScript transpilation |
| `thiserror` | `rho-core`, `rho-ext` | Error type derivation |
| `futures` | `rho-ai` | Stream traits for SSE |
| `sqlx` | `rho-memory` | SQLite + FTS5 for the knowledge base |
## What this buys you

The layered structure means:

- **`rho-core` is UI-agnostic** — no shell, no filesystem, no terminal. Any frontend (REPL, TUI, editor integration) consumes it through the JSON-RPC 2.0 protocol.
- **Tools are pluggable** — `ToolRegistry` stores `Box<dyn Tool>`, so adding a tool is `registry.register(Box::new(MyTool))`.
- **Testing is isolated** — `rho-test-helpers` mocks the core traits (`MockChatClient`, `MockShellExecutor`, `ApprovalGate`) so tool and agent loop tests run without a model server.