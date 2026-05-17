# rho-coding-agent — Plan Directory

This directory contains the long-term development plan for rho-coding-agent.

## Documents

| File | Purpose |
|---|---|
| [roadmap.md](roadmap.md) | Phased development plan with architecture, crate layout, and milestones |
| [phases/phase-1a-COMPLETE/](phases/phase-1a-COMPLETE/) | The agent-loop machinery ✅ |
| [phases/phase-1b-COMPLETE/](phases/phase-1b-COMPLETE/) | The security surface (approval, sandbox, trust, redaction) ✅ |
| [phases/phase-2-COMPLETE/](phases/phase-2-COMPLETE/) | PowerShell-native tools, config, cross-platform support, and security hardening ✅ |
| [phases/phase-2.5-COMPLETE/](phases/phase-2.5-COMPLETE/) | Adaptive-resolution context (session tree, calibrated budget, amnesia fix) ✅ |
| [phases/phase-3-COMPLETE/](phases/phase-3-COMPLETE/) | Rust tooling and tree-sitter ✅ |
| [phases/phase-3.4-COMPLETE/](phases/phase-3.4-COMPLETE/) | First-class frontier model support (shared bootstrapping, CLI flags, egress) ✅ |
| [phases/phase-3.5/](phases/phase-3.5/) | Rust standard library reference (local rustdoc lookup) 🔜 Planned |
| [phases/phase-3.6/](phases/phase-3.6/) | crates.io registry research (crate lookup, search, deps) 🔜 Planned |
| [phases/phase-3.7-COMPLETE/](phases/phase-3.7-COMPLETE/) | Multi-model benchmark harness (`rho-bench` binary) ✅ |
| [phases/phase-3.8-COMPLETE/](phases/phase-3.8-COMPLETE/) | Streaming API, SSE parsing, external provider support ✅ |
| [phases/phase-4/](phases/phase-4/) | Terminal UI 🔜 In Progress |
| [phases/phase-5/](phases/phase-5/) | Extensions and polish |
| [phases/phase-6/](phases/phase-6/) | LSP integration (deferred) |

Phase 1 was originally a single phase. It has been split into 1a (agent-loop machinery) and 1b (security surface) so each gets focused implementation and test coverage. Phase 2.5 (adaptive-resolution context) was added between Phase 2 and Phase 3 to address a shipping amnesia bug and replace the flat conversation model with a tree-shaped session.

## Guiding Principles

1. **Cross-platform with PowerShell as the primary shell.** The agent runs on Windows, macOS, and Linux. PowerShell 7+ (`pwsh`) is the primary shell on all platforms; Windows PowerShell 5.1 (`powershell`) is the fallback on Windows only. The original Windows-only focus was softened after real-world testing showed developers commonly work across platforms.
2. **Rust tooling is a first-class capability.** The agent doesn't just run shell commands — it speaks to `cargo`, `rustc`, and `rust-analyzer` in their native structured formats (JSON messages, LSP).
3. **A clean data model comes first.** Types are the API of the system. If the domain types are easy to construct, compose, and extend, everything built on top of them will be too. If they're awkward, everything will be awkward.
4. **Layered architecture.** Each workspace crate is a layer with a clear dependency direction. No upward dependencies. The kernel (`rho-core`) knows nothing about TUIs or specific tools.
5. **Incremental delivery.** Each phase produces a runnable agent. No big-bang rewrites.
6. **Test-driven development.** Every feature begins with a failing test. Tests define the contract before the implementation exists. At the end of each phase, the test suite is audited and refactored — deduplicating helpers, promoting shared fixtures, and ensuring the suite is maintainable for the long haul.
7. **Minimal dependencies.** Every external dependency is a commitment — to its API, its bugs, its update cadence, and its transitive dependency tree. Before adding a crate, ask: can we write this ourselves in a reasonable amount of code? If yes, write it. If no, depend on it — but understand what we're committing to. Non-negotiable foundations like `tokio`, `serde`, and `reqwest` are accepted. Everything else is scrutinized.
8. **Tree-sitter throughout.** Syntax awareness isn't a display concern tacked on at the TUI layer — it's a structural capability. Tree-sitter is used for syntax highlighting in the terminal, code understanding in tools, and structural navigation in the data model. Introduced in Phase 3, when the first real consumer appears.
