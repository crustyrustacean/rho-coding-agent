# rho-coding-agent — Plan Directory

This directory contains the long-term development plan for rho-coding-agent.

## Documents

| File | Purpose |
|---|---|
| [roadmap.md](roadmap.md) | Phased development plan with architecture, crate layout, and milestones |

## Guiding Principles

1. **Windows and PowerShell are first-class.** The agent assumes PowerShell as its shell, Windows paths as its native format, and Windows tooling as its baseline. Unix support is welcome but not the priority.
2. **Rust tooling is a first-class capability.** The agent doesn't just run shell commands — it speaks to `cargo`, `rustc`, and `rust-analyzer` in their native structured formats (JSON messages, LSP).
3. **A clean data model comes first.** Types are the API of the system. If the domain types are easy to construct, compose, and extend, everything built on top of them will be too. If they're awkward, everything will be awkward.
4. **Layered architecture.** Each workspace crate is a layer with a clear dependency direction. No upward dependencies. The kernel (`rho-core`) knows nothing about TUIs or specific tools.
5. **Incremental delivery.** Each phase produces a runnable agent. No big-bang rewrites.
6. **Tree-sitter throughout.** Syntax awareness isn't a display concern tacked on at the TUI layer — it's a structural capability. Tree-sitter is used for syntax highlighting in the terminal, code understanding in tools, and structural navigation in the data model.
