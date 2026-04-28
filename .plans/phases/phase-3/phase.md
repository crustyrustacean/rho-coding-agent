# Phase 3: Rust Tooling

**Goal:** The agent understands Rust compilation errors and Clippy lints as structured data, not text. It can fix code using the compiler's own suggestions.

**Milestone:** The agent runs `cargo check`, parses the JSON diagnostics, applies the machine-applicable fix, and verifies the fix compiles.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| None | — | — |

All Rust tooling shells out to `cargo`/`rustc` via `tokio::process::Command` (already available). JSON message parsing uses `serde_json` (already available). The diagnostic types are our own data model. No new crates needed.

## Exit Criteria

The agent can diagnose and fix compilation errors using structured compiler output. It prefers compiler suggestions over its own guesses. `EditFile` warns on syntax-node-splitting replacements. `rho-eval` can score the agent on a suite of canonical tasks.
