# Dependency Philosophy

rho is conservative about adding external dependencies. Every new dependency is a supply-chain commitment that affects build times, binary size, audit surface, and maintenance burden.

## Decision framework

Before adding a dependency, answer these questions:

1. **Can we write it ourselves in < 100 lines?** → Write it. rho prefers small, purpose-built implementations over pulling in a crate for something trivial.

2. **Is it already a transitive dependency?** → If `reqwest` already pulls in `serde_urlencoded`, we can use it directly without adding a new top-level dependency.

3. **Is it the de-facto standard for the problem?** → `serde`, `tokio`, `clap`, `tracing` are widely adopted, well-maintained, and unlikely to cause issues. Standard choices are preferred over novel alternatives.

4. **Is the maintenance track record good?** → Check last release date, open issues, contributor count. A crate that hasn't been updated in 2 years is a risk.

## Current dependencies

rho's direct (non-transitive) dependencies are:

| Crate | Purpose | Rationale |
|---|---|---|
| `reqwest` | HTTP client | De-facto standard; needed for model API |
| `tokio` | Async runtime | De-facto standard; needed for async I/O |
| `serde` + `serde_json` | Serialization | Essential; no practical alternative |
| `toml` | Config parsing | Standard for TOML in Rust |
| `clap` | CLI parsing | De-facto standard; derives-based API |
| `tracing` | Structured logging | De-facto standard; `log`-compatible |
| `async-trait` | Async trait support | Required for `Box<dyn Tool>` |
| `thiserror` | Error derive macros | Clean `RhoError` definition |
| `ignore` | `.gitignore` walking | From ripgrep; mature, well-maintained |
| `tree-sitter` + grammars | Syntax analysis | Standard for tree-sitter in Rust |
| `which` | Executable detection | Small, focused, no replacement needed |

## What we don't depend on

| Avoided | Why |
|---|---|
| `scraper` (HTML parsing) | Not yet needed; regex-based extraction suffices for now |
| `ureq` (minimal HTTP) | `reqwest` already in tree; adding a second HTTP client is fragmentation |
| `anyhow` | Error handling | Removed from library crates; domain-specific error types used instead |
| `rand` | UUID generation uses `uuid` crate only in test helpers (dev-only) |
| `clap-cargo` | Manual `cargo_metadata` integration avoided; `--version` from `clap` derive |