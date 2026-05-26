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

## Tooling

- **git-cliff** — generates `CHANGELOG.md` from conventional commits. Invoked by `cargo xtask changelog <version>`.
- **cargo-release** — automates version bumping, tagging, and publishing. Run `cargo release <version>` to perform a dry-run; add `--execute` to apply.

## Hashline Editing

`read_file` returns content in hashline format: each line is prefixed with `LINE#HASH:`.

```
<context>
 8#VR:function hello() {
 9#KT:  console.log("world");
10#BH:}
<context:end>
```

`edit_file` accepts hashline anchors for precise, content-verified edits:

```json
{"op": "replace", "pos": "9#KT", "lines": ["  console.log('updated');"]}
```

Operations: `replace`, `append`, `prepend`, `delete`. Hash mismatches fail with fresh hashes for retry.

The hash is a 2-character string from alphabet `ZPMQVRWSNKTXJBYH` (256 combinations), computed from line content (or line number for non-alphanumeric lines). Implemented in `rho-tools/src/hashline.rs`.

Legacy `old_text`/`new_text` edits still work but prefer hashline.

## Release

```sh
# One command — runs CI, bumps version, updates changelog, tags, commits:
cargo xtask release 0.48.0       # explicit version
# or:
cargo xtask release patch        # bump current version

# If you just ran CI:
cargo xtask release patch --skip-ci

# Then push:
git push origin trunk --tags
```

The `release` task:
1. Runs `cargo xtask ci` (unless `--skip-ci`).
2. Bumps `workspace.package.version` in `Cargo.toml`.
3. Runs `git cliff --tag v<version> --prepend CHANGELOG.md` (existing entries preserved).
4. Creates a `v<version>` git tag.
5. Commits with `chore(release): prepare <version>`.

Tags are required by `git-cliff` to delimit releases. The `release` task creates them automatically.

To regenerate the changelog without a release (e.g. after editing commit messages):

```sh
cargo xtask changelog    # reads version from Cargo.toml, prepends entry
```
