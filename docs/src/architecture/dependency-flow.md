# Dependency Flow

Dependencies flow downward only. A crate may depend on crates below it in the stack but never above.

```
rho (binary) ──────────────────────────────────────────
    │
    ├── rho-tools ─────────────────────────────────────
    │       │
    │       └── rho-highlight (via EditFile node-splitting)
    │
    └── rho-core ─────────────────────────────────────
            │
rho-bench ──────── rho-eval ── rho-core ─────────────
rho-test-helpers ── rho-core ────────────────────────
xtask ──────────────────────────────────────────────
```

| Crate | Depends on | Notes |
|---|---|---|
| `rho` | `rho-core`, `rho-tools` | Binary entry point — wires everything together |
| `rho-tools` | `rho-core`, `rho-highlight` | Tool implementations use core types and highlight for node-splitting |
| `rho-highlight` | none (external only) | Tree-sitter grammar — standalone, no rho dependencies |
| `rho-core` | none (external only) | Kernel — the foundation everything else builds on |
| `rho-test-helpers` | `rho-core` | Dev-only — provides mocks and fixtures for testing |
| `rho-eval` | `rho-core` | Dev-only — benchmark task definitions and scoring |
| `rho-bench` | `rho-core`, `rho-eval`, `rho-tools` | Dev-only — benchmark harness binary for multi-model evaluation |
| `xtask` | none (cargo integration) | Dev-only — task runner, no rho crate dependencies |

## Key external dependencies

| Dependency | Used by | Purpose |
|---|---|---|
| `reqwest` | `rho-core` | HTTP client for model API |
| `tokio` | `rho-core`, `rho-tools` | Async runtime |
| `serde` / `serde_json` | everywhere | Serialization |
| `toml` | `rho-core` | Configuration parsing |
| `tree-sitter` + grammars | `rho-highlight` | Syntax analysis |
| `ignore` | `rho-tools` | `.gitignore`-aware directory listing |
| `clap` | `rho`, `rho-bench` | CLI argument parsing |
| `tracing` | `rho-core`, `rho-bench` | Structured logging |
| `chrono` | `rho-bench`, `rho-eval` | ISO 8601 timestamps in results |
| `thiserror` | `rho-core` | Error type derivation |
## What this buys you

The layered structure means:

- **`rho-core` is UI-agnostic** — no shell, no filesystem, no terminal. The TUI (Phase 4) and REPL both consume it through the same traits.
- **Tools are pluggable** — `ToolRegistry` stores `Box<dyn Tool>`, so adding a tool is `registry.register(Box::new(MyTool))`.
- **Testing is isolated** — `rho-test-helpers` mocks the core traits (`ChatClient`, `ShellExecutor`, `ApprovalGate`) so tool and agent loop tests run without a model server.