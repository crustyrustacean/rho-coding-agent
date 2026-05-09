# Roadmap

rho is developed in phases, each building on the last. The current version is **0.31.1**.

## Phase summary

| Phase | Status | Summary |
|---|---|---|
| 1a: The Agent Loop | ✅ Complete | Agent loop, tool registry, `ChatClient` trait, conversation management |
| 1b: Security Surface | ✅ Complete | Approval gate, file sandbox, context-file trust, secret redaction, untrusted-data framing |
| 2: Shell, File Tools & Cross-Platform | ✅ Complete | PowerShell-native shell, file tools, config loader, command denylist, egress allowlist, cross-platform |
| 2.5: Adaptive-Resolution Context | ✅ Complete | Session tree, resolution levels, calibrated budget, tool-result bounding, amnesia fix, JSONL persistence |
| 3: Rust Tooling and Tree-Sitter | ✅ Complete | `rho-highlight`, structured diagnostics, `CargoCheck`/`Clippy`/`Test`/`Fix`/`RustcExplain`, node-splitting validation, `rho-eval` benchmark suite, `rho-bench` multi-model harness |
| 3.5: Rust Standard Library Reference | 📋 Planned | Local rustdoc lookup tool for stdlib API docs |
| 3.6: crates.io Research | 📋 Planned | Crate metadata lookup, search, version history, dependency inspection |
| 4: Terminal UI | 🔜 Next | Rich TUI replacing the bare REPL |
| 5: Extensions and Polish | Planned | Custom tools, prompt composition with budget awareness |
| 6: LSP | Deferred | rust-analyzer integration |

## Completed phases

### Phase 1a — The Agent Loop

The foundation: `Tool` trait, `ToolRegistry`, `ChatClient` trait, `Conversation`, and the `run_loop` state machine. The agent can send messages to a model, receive tool calls, execute them, and feed results back.

### Phase 1b — Security Surface

Defense-in-depth: file sandbox, approval gate (per-tool, risk-based), secret redaction, untrusted-data framing with `<context>` tags, and project context file trust with SHA-256 hash verification.

### Phase 2 — Shell, File Tools & Cross-Platform

PowerShell-native execution on all platforms, `ReadFile`/`WriteFile`/`ListDir`/`EditFile` tools, two-tier TOML config, command denylist, egress allowlist, compact prompt for small-context models, and full cross-platform support (Windows/macOS/Linux).

### Phase 2.5 — Adaptive-Resolution Context

Session tree (replacing flat `Conversation`), resolution levels (`Full`/`Compacted`/`Attached`), calibrated token estimation with exponential moving average, tool-result bounding to prevent single-output context overflow, and JSONL persistence with auto-flush.

### Phase 3 — Rust Tooling and Tree-Sitter

`rho-highlight` crate with tree-sitter parsing, `node_at()` position lookup, and token classification. Structured compiler diagnostics (`CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `CargoExplain`). `EditFile` node-splitting validation. `rho-eval` benchmark suite with 5 validated end-to-end scenarios. `rho-bench` harness for multi-model comparison with token usage, wall time, and JSON result persistence.

## Upcoming phases

### Phase 3.5 — Rust Standard Library Reference

A `RustdocLookup` tool that reads locally installed rustdoc HTML via `rustup doc --path`. Zero network dependency. Supports type, method, trait, and function lookups. See [Phase 3.5 planning note](../../../1.%20Planning/Phase%203.5/Phase%203.5%20—%20Rust%20Standard%20Library%20Reference.md).

### Phase 3.6 — crates.io Research

A `CratesIoLookup` tool for crate metadata, search, version history, and dependency tree inspection. Requires egress allowlist opt-in. Dedicated Rust HTTP client (not a shell escape). See [Phase 3.6 planning note](../../../1.%20Planning/Phase%203.6/Phase%203.6%20—%20crates.io%20Registry%20Research.md).

### Phase 4 — Terminal UI

Rich TUI replacing the bare REPL. Streaming output, approval prompts with rich previews, diagnostic panels, syntax highlighting. Built on `ratatui`/`crossterm`. See [Phase 4 readiness assessment](../../../1.%20Planning/Phase%204/Phase%204%20Readiness%20Assessment.md).

### Phase 5 — Extensions and Polish

Custom tools via TOML definitions, extension API (`rho-ext` crate), prompt composition with budget awareness, and custom slash commands.

### Phase 6 — LSP

`rust-analyzer` integration for real-time diagnostics, go-to-definition, and refactoring support. Deferred until after the TUI is stable.
