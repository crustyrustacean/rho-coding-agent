# Roadmap

rho is developed in phases, each building on the last. The current version is **0.46.0**.

## Phase summary

| Phase | Status | Summary |
|---|---|---|
| 1a: The Agent Loop | ✅ Complete | Agent loop, tool registry, `ChatClient` trait, conversation management |
| 1b: Security Surface | ✅ Complete | Approval gate, file sandbox, context-file trust, secret redaction, untrusted-data framing |
| 2: Shell, File Tools & Cross-Platform | ✅ Complete | PowerShell-native shell, file tools, config loader, command denylist, cross-platform |
| 2.5: Adaptive-Resolution Context | ✅ Complete | Session tree, resolution levels, calibrated budget, tool-result bounding, amnesia fix, JSONL persistence |
| 3: Rust Tooling and Tree-Sitter | ✅ Complete | `rho-highlight`, structured diagnostics, `CargoCheck`/`Clippy`/`Test`/`Fix`/`RustcExplain`, node-splitting validation, `rho-eval` benchmark suite, `rho-bench` multi-model harness |
| 3.5: Rust Standard Library Reference | ✅ Complete | Local rustdoc lookup tool for stdlib API docs |
| 3.6: crates.io Research | ✅ Complete | Crate metadata lookup, search, version history, dependency inspection |
| 3.8: Hashline Editing | ✅ Complete | Content-addressed line references with fuzzy anchor matching |
| 3.9: Streaming & Live Output | ✅ Complete | SSE streaming, `AgentObserver` trait, REPL live output, `ReplObserver` |
| 3.10: Multi-Provider & Model Picker | ✅ Complete | `Provider` trait, `ProviderRegistry`, interactive model picker, `/models`, `/model` |
| 3.11: Session Discovery & Context Visibility | ✅ Complete | `find_latest_session()`, `list_sessions()`, `rho -c`, `/sessions`, `/status`, context status bar |
| 4: Terminal UI | 🔜 Next | Rich TUI replacing the bare REPL |
| 5: Extensions and Polish | Planned | Custom tools, prompt composition with budget awareness |
| 6: LSP | Deferred | rust-analyzer integration |

## Completed phases

### Phase 1a — The Agent Loop

The foundation: `Tool` trait, `ToolRegistry`, `ChatClient` trait, `Conversation`, and the `run_loop` state machine. The agent can send messages to a model, receive tool calls, execute them, and feed results back.

### Phase 1b — Security Surface

Defense-in-depth: file sandbox, approval gate (per-tool, risk-based), secret redaction, untrusted-data framing with `<context>` tags, and project context file trust with SHA-256 hash verification.

### Phase 2 — Shell, File Tools & Cross-Platform

PowerShell-native execution on all platforms, `ReadFile`/`WriteFile`/`ListDir`/`EditFile` tools, two-tier TOML config, command denylist, compact prompt for small-context models, and full cross-platform support (Windows/macOS/Linux).

### Phase 2.5 — Adaptive-Resolution Context

Session tree (replacing flat `Conversation`), resolution levels (`Full`/`Compacted`/`Attached`), calibrated token estimation with exponential moving average, tool-result bounding to prevent single-output context overflow, and JSONL persistence with auto-flush.

### Phase 3 — Rust Tooling and Tree-Sitter

`rho-highlight` crate with tree-sitter parsing, `node_at()` position lookup, and token classification. Structured compiler diagnostics (`CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `CargoExplain`). `EditFile` node-splitting validation. `rho-eval` benchmark suite with 5 validated end-to-end scenarios. `rho-bench` harness for multi-model comparison with token usage, wall time, and JSON result persistence.

### Phase 3.5 — Rust Standard Library Reference

A `RustdocLookup` tool that reads locally installed rustdoc HTML via `rustup doc --path`. Zero network dependency. Supports type, method, trait, and function lookups.

### Phase 3.6 — crates.io Research

A `CratesIoLookup` tool for crate metadata, search, version history, and dependency tree inspection. Dedicated Rust HTTP client (not a shell escape).

### Phase 3.8 — Hashline Editing

Content-addressed line references (`LINE#HASH:`) for reliable file editing. ReadFile returns content in hashline format. EditFile supports hashline-anchor operations (replace, append, prepend, delete). Fuzzy anchor matching for resilience when the file has changed since the last read.

### Phase 3.9 — Streaming & Live Output

SSE streaming for the Chat Completions API. `AgentObserver` trait with 7 event methods for real-time progress rendering. `NopObserver` for tests/benchmarks. `ReplObserver` streams reasoning deltas and tool activity to stdout in the REPL.

### Phase 3.10 — Multi-Provider & Model Picker

`Provider` trait for unified provider abstraction. `ProviderRegistry` manages multiple providers and routes model requests. Interactive model picker with curated models from Anthropic, OpenAI, and z.ai when auto-detection fails. `/models` and `/model` REPL commands for browsing and switching models at runtime.

### Phase 3.11 — Session Discovery & Context Visibility

Session discovery functions (`find_latest_session`, `list_sessions`) for lightweight header-only session scanning. `rho -c` / `--continue` flag to auto-resume the most recent session. `/sessions` REPL command listing recent sessions with timestamps, sizes, and entry counts. `ContextStats` struct for context window usage snapshots. Live status bar after every REPL turn with color-coded utilization. `/status` REPL command for detailed context breakdown.

## Upcoming phases

### Phase 4 — Terminal UI

Rich TUI replacing the bare REPL. Streaming output, approval prompts with rich previews, diagnostic panels, syntax highlighting. Built on `ratatui`/`crossterm`. See [Phase 4 readiness assessment](../../../1.%20Planning/Phase%204/Phase%204%20Readiness%20Assessment.md).

### Phase 5 — Extensions and Polish

Custom tools via TOML definitions, extension API (`rho-ext` crate), prompt composition with budget awareness, and custom slash commands.

### Phase 6 — LSP

`rust-analyzer` integration for real-time diagnostics, go-to-definition, and refactoring support. Deferred until after the TUI is stable.
