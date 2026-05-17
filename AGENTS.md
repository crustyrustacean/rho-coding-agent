# AGENTS.md

Guidance for AI assistants working on this codebase.

For architecture, key types, and crate responsibilities, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Quick Start

```sh
cargo xtask ci    # Run the full CI pipeline before considering work done
```

This runs `fmt → lint → build → test` in sequence. All four must pass.

## Coding Conventions

- **Edition:** Rust 2024.
- **Clippy:** Pedantic + cargo lints are enforced (`-D warnings`). Missing docs on private items and error variants are warnings.
- **Formatting:** `cargo fmt --all -- --check` must pass.
- **Commits:** [Conventional commits](https://www.conventionalcommits.org/) — `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- **Versioning:** Bump the version in the workspace `Cargo.toml` only. It propagates via `workspace = true`.

## Testing

```sh
cargo xtask test                # Run all tests
cargo xtask test -- --nocapture # Run with stdout visible
```

- Unit tests live in `#[cfg(test)] mod tests` blocks within each source file.
- Integration tests live in `rho-core/tests/`.
- Tool integration tests live in `rho-tools/tests/`.
- `rho-test-helpers` provides `MockChatClient`, `MockShellExecutor`, response builders (`text_response`, `tool_call_response`, `multi_tool_call_response`), approval gates (`AutoApproveGate`, `AutoDenyGate`), file-system test environment (`FileTestEnv`), shell detection (`detect_shell`), sandbox helpers (`tempdir_with_sandbox`), and trust-store helpers (`empty_trust_store`).
- `rho-eval` defines canonical coding tasks and scoring logic used by `rho-bench`.
- `rho-bench` runs eval tasks against models with timing and token metrics. Run with `cargo run -p rho-bench -- --models <id> --endpoint <url>`.
- When adding new deserialization logic, add a JSON fixture test.
- For `Conversation` branching logic, prefer the trait-abstraction pattern over coupling to `LocalChatClient`.

## Release Checklist

1. Ensure `cargo xtask ci` passes.
2. `cargo xtask changelog <version>` — update `CHANGELOG.md`.
3. Bump `version` in workspace `Cargo.toml`.
4. `git add -A && git commit -m "chore(release): prepare <version>"`.
5. `git push origin trunk`.
