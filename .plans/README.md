# rho-coding-agent — Plan Directory

This directory contains the long-term development plan for rho-coding-agent.

## Documents

| File | Purpose |
|---|---|
| [roadmap.md](roadmap.md) | Phased development plan with architecture, crate layout, and milestones |
| [phases/phase-1a/](phases/phase-1a/) | The agent-loop machinery |
| [phases/phase-1b/](phases/phase-1b/) | The security surface (approval, sandbox, trust, redaction) |
| [phases/phase-2/](phases/phase-2/) | PowerShell-native tools, config, cross-platform support, and security hardening |
| [phases/phase-3/](phases/phase-3/) | Rust tooling and tree-sitter |
| [phases/phase-4/](phases/phase-4/) | Terminal UI |
| [phases/phase-5/](phases/phase-5/) | Extensions and polish |
| [phases/phase-6/](phases/phase-6/) | LSP integration (deferred) |

Phase 1 was originally a single phase. It has been split into 1a (agent-loop machinery) and 1b (security surface) so each gets focused implementation and test coverage.

## Guiding Principles

1. **Windows and PowerShell are first-class.** The agent assumes PowerShell as its shell, Windows paths as its native format, and Windows tooling as its baseline. Unix support is welcome but not the priority.
2. **Rust tooling is a first-class capability.** The agent doesn't just run shell commands — it speaks to `cargo`, `rustc`, and `rust-analyzer` in their native structured formats (JSON messages, LSP).
3. **A clean data model comes first.** Types are the API of the system. If the domain types are easy to construct, compose, and extend, everything built on top of them will be too. If they're awkward, everything will be awkward.
4. **Layered architecture.** Each workspace crate is a layer with a clear dependency direction. No upward dependencies. The kernel (`rho-core`) knows nothing about TUIs or specific tools.
5. **Incremental delivery.** Each phase produces a runnable agent. No big-bang rewrites.
6. **Test-driven development.** Every feature begins with a failing test. Tests define the contract before the implementation exists. At the end of each phase, the test suite is audited and refactored — deduplicating helpers, promoting shared fixtures, and ensuring the suite is maintainable for the long haul.
7. **Minimal dependencies.** Every external dependency is a commitment — to its API, its bugs, its update cadence, and its transitive dependency tree. Before adding a crate, ask: can we write this ourselves in a reasonable amount of code? If yes, write it. If no, depend on it — but understand what we're committing to. Non-negotiable foundations like `tokio`, `serde`, and `reqwest` are accepted. Everything else is scrutinized.
8. **Tree-sitter throughout.** Syntax awareness isn't a display concern tacked on at the TUI layer — it's a structural capability. Tree-sitter is used for syntax highlighting in the terminal, code understanding in tools, and structural navigation in the data model. Introduced in Phase 3, when the first real consumer appears.
