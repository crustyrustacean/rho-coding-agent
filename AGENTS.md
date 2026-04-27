# AGENTS.md

Guidance for AI assistants working on this codebase.

## Quick Start

```sh
cargo xtask ci    # Run the full CI pipeline before considering work done
```

This runs `fmt → lint → build → test` in sequence. All four must pass.

## Project Layout

```
rho-core/           # Core library + binary (`rho`)
  src/
    lib.rs          # Domain types, HTTP client, Conversation — the public API
    bin/main.rs     # CLI entry point (clap, REPL loop)
  Cargo.toml        # Dependencies: reqwest, serde, clap, tokio, thiserror, anyhow
xtask/              # Dev task runner (cargo-xtask pattern, not published)
  src/
    main.rs         # CLI dispatch
    tasks.rs        # Task implementations
Cargo.toml          # Workspace root — version is set here and inherited by members
CHANGELOG.md        # Generated via git-cliff
cliff.toml          # git-cliff configuration
```

## Architecture

`rho-core` is an OpenAI-compatible chat completions client. The core flow is:

1. **`Conversation`** accumulates `ChatMessage`s (with optional system prompt and tool definitions).
2. **`Conversation::send`** appends a user message, POSTs to the model API via `RhoHttpClient`, and returns an `AssistantResponse`.
3. **`AssistantResponse`** is either a `Message` (text reply, auto-appended to history) or a `ToolCall` (model wants to invoke a tool — caller must execute and feed the result back).

The binary (`rho`) is a simple REPL that prints text replies or tool call requests to stdout.

## Coding Conventions

- **Edition:** Rust 2024.
- **Clippy:** Pedantic + cargo lints are enforced (`-D warnings`). Missing docs on private items and error variants are warnings.
- **Formatting:** `cargo fmt --all -- --check` must pass.
- **Commits:** [Conventional commits](https://www.conventionalcommits.org/) — `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- **Versioning:** Bump the version in the workspace `Cargo.toml` only. It propagates via `workspace = true`.

## Key Types

| Type | Purpose |
|---|---|
| `ChatRequest` | Request body sent to the model API |
| `ChatMessage` | A single message with a `Role` and content |
| `Tool` / `ToolFunction` / `ToolParameters` | Tool definitions sent with requests |
| `ModelResponse` | Parsed API response |
| `FinishReason` | Why the model stopped (`Stop`, `ToolCalls`, `Length`, `ContentFilter`) |
| `ModelToolCall` / `ToolCallFunction` | A tool invocation requested by the model |
| `AssistantResponse` | Result of `Conversation::send` — `Message` or `ToolCall` |
| `RhoError` | Error enum (`Http`, `Json`, `Unexpected`) |
| `Conversation` | Message history + model ID + tool list |
| `RhoHttpClient` | HTTP client wrapping `reqwest` |

## Testing

```sh
cargo xtask test                # Run all tests
cargo xtask test -- --nocapture # Run with stdout visible
```

Tests exist in `rho-core/src/lib.rs` under `#[cfg(test)] mod tests`. When adding new deserialization logic, add a JSON fixture test. For `Conversation` branching logic, prefer the trait-abstraction pattern over coupling to `RhoHttpClient`.

## Release Checklist

1. Ensure `cargo xtask ci` passes.
2. `cargo xtask changelog <version>` — update `CHANGELOG.md`.
3. Bump `version` in workspace `Cargo.toml`.
4. `git add -A && git commit -m "chore(release): prepare <version>"`.
5. `git push origin trunk`.
