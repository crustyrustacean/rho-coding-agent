# Roadmap

rho is developed in phases, each building on the last. The current version is **0.84.7**.

## Phase summary

| Phase | Status | Summary |
|---|---|---|
| 1a: The Agent Loop | ✅ Complete | Agent loop, tool registry, `LlmService` trait, conversation management |
| 1b: Security Surface | ✅ Complete | Approval gate, file sandbox, context-file trust, secret redaction, untrusted-data framing |
| 2: Shell, File Tools & Cross-Platform | ✅ Complete | PowerShell-native shell, file tools, config loader, command denylist, cross-platform |
| 2.5: Adaptive-Resolution Context | ✅ Complete | Session tree, resolution levels, calibrated budget, tool-result bounding, amnesia fix, JSONL persistence |
| 3: Rust Tooling and Tree-Sitter | ✅ Complete | `rho-highlight`, structured diagnostics, `CargoCheck`/`Clippy`/`Test`/`Fix`/`RustcExplain`, node-splitting validation |
| 3.5: Rust Standard Library Reference | ✅ Complete | Local rustdoc lookup tool for stdlib API docs |
| 3.6: crates.io Research | ✅ Complete | Crate metadata lookup, search, version history, dependency inspection |
| 3.8: Hashline Editing | ✅ Complete | Content-addressed line references with fuzzy anchor matching |
| 3.9: Streaming & Live Output | ✅ Complete | SSE streaming, `AgentObserver` trait, streaming output via JSON-RPC notifications |
| 3.10: Multi-Provider & Model Picker | ✅ Complete | `Provider` trait, `ProviderRegistry`, `/models`, `/model` |
| 3.11: Session Discovery & Context Visibility | ✅ Complete | `find_latest_session()`, `list_sessions()`, `rho -c`, `/sessions`, `/status`, context status bar |
| 3.12: RPC Mode | ✅ Complete | Headless JSON-RPC 2.0 over stdin/stdout, `run_rpc_on<R, W>`, approval round-trips, integration tests |
| 4: Terminal UI | 🔜 Next | Rich TUI replacing the bare REPL |
| 5: TypeScript Extensions | ✅ Complete | `rho-ext` crate, V8/deno-core runtime, `ExtensionLoader`, `DenoTool`, `DenoObserver`, hot reload, config integration, type definitions |
| 6: LSP | Deferred | rust-analyzer integration |

## Context Management Enhancement (0.62–0.64)

A series of incremental improvements to context management, addressing structural issues that caused context exhaustion and amnesia:

| Phase | Summary | Version |
|---|---|---|
| Graduated Resolution | Added `Outlined` and `Summarized` intermediate fidelity levels | 0.62 |
| Mechanical Outlining | 12 tool-specific structural summary formatters | 0.62 |
| Selective Turn-Internal Eviction | Per-entry downgrade planner (Full → Outlined → Summarized) | 0.63 |
| Phase Detection | `SessionPhase` state machine tracking exploration/execution/verification | 0.63 |
| Phase-Aware Compaction | Narrative summaries grouped by session phase | 0.64 |
| LLM Compaction | Model-generated narrative notes (opt-in, graceful fallback) | 0.64 |
| Enhanced ContextStats | Token distribution by role, resolution, and phase | 0.64 |
| Config fields removed | `context_pressure_threshold`, `context_pressure_interval` removed from code; structural mechanisms handle context management | 0.81 |

## Upcoming phases

### Phase 4 — Terminal UI

Rich TUI replacing the bare REPL. Streaming output, approval prompts with rich previews, diagnostic panels, syntax highlighting. Built on `ratatui`/`crossterm`.

### Phase 6 — LSP

`rust-analyzer` integration for real-time diagnostics, go-to-definition, and refactoring support. Deferred until after the TUI is stable.