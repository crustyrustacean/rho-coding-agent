# Dependency Flow

Dependencies flow downward only. A crate may depend on crates below it in the stack but never above.

```
rho (binary) ──────────────────────────────────────────
    │
    ├── rho-ext ──── rho-core ──── rho-ai ──────────────
    │
    ├── rho-tools ─────────────────────────────────────
    │       │
    │       └── rho-highlight (via EditFile node-splitting)
    │
    └── rho-core ─────────────────────────────────────
            │
rho-repl ──── (no rho deps, spawns rho as subprocess)
rho-bench ──────── rho-eval ── rho-core ─────────────
rho-test-helpers ── rho-core, rho-ai ────────────────
xtask ──────────────────────────────────────────────
```

| Crate | Depends on | Notes |
|---|---|---|
| `rho` | `rho-core`, `rho-tools`, `rho-ext`, `rho-ai` | Headless JSON-RPC 2.0 agent |
| `rho-repl` | none (external only) | Interactive terminal client — spawns `rho` as a subprocess |
| `rho-ext` | `rho-core`, `deno_core`, `deno_ast` | TypeScript extension runtime |
| `rho-tools` | `rho-core`, `rho-highlight` | Tool implementations use core types and highlight for node-splitting |
| `rho-highlight` | none (external only) | Tree-sitter grammar — standalone, no rho dependencies |
| `rho-core` | `rho-ai` | Kernel — the foundation everything else builds on |
| `rho-ai` | none (external only) | Unified LLM provider abstraction |
| `rho-test-helpers` | `rho-core`, `rho-ai` | Dev-only — provides mocks and fixtures for testing |
| `rho-eval` | `rho-core` | Dev-only — benchmark task definitions and scoring |
| `rho-bench` | `rho-core`, `rho-eval`, `rho-tools` | Dev-only — benchmark harness binary for multi-model evaluation |
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
| `clap` | `rho`, `rho-bench` | CLI argument parsing |
| `tracing` | `rho-core`, `rho-ext`, `rho-bench` | Structured logging |
| `deno_core` | `rho-ext` | V8 JavaScript runtime |
| `deno_ast` | `rho-ext` | TypeScript transpilation |
| `chrono` | `rho-bench`, `rho-eval` | ISO 8601 timestamps in results |
| `thiserror` | `rho-core`, `rho-ext` | Error type derivation |
| `futures` | `rho-ai` | Stream traits for SSE |
## What this buys you

The layered structure means:

- **`rho-core` is UI-agnostic** — no shell, no filesystem, no terminal. The REPL (`rho-repl`) and any future TUI both consume it through the same JSON-RPC 2.0 protocol.
- **Tools are pluggable** — `ToolRegistry` stores `Box<dyn Tool>`, so adding a tool is `registry.register(Box::new(MyTool))`.
- **Testing is isolated** — `rho-test-helpers` mocks the core traits (`MockChatClient`, `MockShellExecutor`, `ApprovalGate`) so tool and agent loop tests run without a model server.