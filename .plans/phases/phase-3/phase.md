# Phase 3: Rust Tooling and Tree-Sitter

**Goal:** The agent understands Rust compilation errors and Clippy lints as structured data, not text. It can fix code using the compiler's own suggestions. Tree-sitter is introduced as a structural-understanding capability, used by `EditFile` and the Rust diagnostic tooling.

**Milestone:** The agent runs `cargo check`, parses the JSON diagnostics, applies the machine-applicable fix, and verifies the fix compiles. `EditFile` warns when an exact-match replacement would split a syntax node.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `tree-sitter` **(foundation)** | `rho-highlight` | Standard parser generator runtime — writing a parser from scratch is not feasible |
| `tree-sitter-rust` **(foundation)** | `rho-highlight` | Rust grammar — this *is* the spec, thousands of rules, not writable by hand |

Tree-sitter was previously scheduled for Phase 1 to "validate the integration path." That justification was not strong enough to pull in two crates and a C-toolchain build dependency before there was a real consumer. Phase 3 is when the first real consumer appears (mapping diagnostic spans to AST nodes; node-splitting validation in `EditFile`), so the scaffolding lands here. Rust tooling itself shells out to `cargo`/`rustc` via `tokio::process::Command` (already available); JSON message parsing uses `serde_json` (already available); the diagnostic types are our own data model.

The Rust grammar pulls in a C compiler at build time. Document this in the project README so contributors know what to install (Visual Studio Build Tools or MSVC on Windows; `gcc`/`cc` on Unix). Cargo features are defined from this phase: `rust` as default, with `powershell`, `toml`, `json`, `markdown` as opt-in features added in Phase 4.

## Exit Criteria

The agent can diagnose and fix compilation errors using structured compiler output. It prefers compiler suggestions over its own guesses. `rho-highlight` parses Rust source via tree-sitter and exposes a structural-query API. `EditFile` warns on syntax-node-splitting replacements. `rho-eval` can score the agent on a suite of canonical tasks.
