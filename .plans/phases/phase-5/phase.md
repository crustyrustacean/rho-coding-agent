# Phase 5: Extensions and Polish

**Goal:** The agent is extensible. Users can add custom tools and integrate their own workflows.

**Milestone:** A user can add a custom tool via config, restart the agent, and the model can use it.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| None | — | — |

Config loading (`toml`) was already introduced in Phase 2. `rho-ext` builds on top of the existing config infrastructure.

## Decisions

**Extension format:** Phase 5 starts with TOML-defined tools (command name + args template). No Lua, no WASM. These add enormous complexity (runtime embedding, sandboxing, FFI) for marginal benefit at this stage. If TOML tools prove too limited, the next step would be Lua via `mlua` — but that's a Phase 6+ decision.

**LSP client:** `rust-analyzer` integration is deferred to Phase 6+. The LSP protocol is complex (JSON-RPC + `Content-Length` framing, `initialize`/`initialized` handshake, capability negotiation, long-lived background process lifecycle) — likely 800–1200 lines of careful code, not the ~500 initially estimated. The agent is already high-value without it. If LSP is pursued later, accept `lsp-types` (the rust-analyzer team's own crate, well-maintained) rather than hand-rolling JSON-RPC — the protocol surface area is too large to reimplement safely.

## Exit Criteria

The agent is configurable and extensible. Users can define custom tools in `.rho/config.toml` and the model can use them. Prompt composition merges base, shell, Rust, and project-specific instructions with budget awareness — the system prompt's token cost is measured, logged, and warned when it exceeds a configurable fraction of the token budget. The agent is production-ready for daily Rust development on Windows.
