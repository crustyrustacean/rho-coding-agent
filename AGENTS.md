# AGENTS.md

Guidance for AI assistants working on this codebase.

For architecture, key types, and crate responsibilities, see the mdBook at [`docs/src/`](docs/src/SUMMARY.md) — the single source of truth.

## Extensions

rho supports TypeScript extensions via `rho-ext`. Extensions live in `~/.rho/extensions/` (user-level) and `.rho/extensions/` (project-level). Each extension gets its own V8 isolate and can provide tools, hooks, and slash commands.

```typescript
// ~/.rho/extensions/hello.ts
export default {
  name: "hello",
  tools: [{
    name: "hello",
    description: "Greet someone",
    risk: "read" as const,
    parameters: {
      name: { type: "string", description: "Who to greet", required: true },
    },
    execute: async (args: string) => {
      return JSON.stringify({ output: `Hello, ${args}!` });
    },
  }],
};
```

Type definitions are shipped at `rho-ext/types/rho.d.ts`.

### REPL commands

- `/reload` — hot-reload extensions from disk (mtime-based change detection)
- `/extensions` — list loaded extension names

### Config

```toml
[extensions]
enabled = ["hello", "crates-search"]

[extensions.defaults]
network = true
max_memory_mb = 64

[extensions.per_extension."hello"]
max_memory_mb = 128
```

See [`docs/src/extensions.md`](docs/src/extensions.md) for the full extension system documentation.

## Quick Start

```sh
cargo xtask ci    # Run the full CI pipeline before considering work done
```

This runs `fmt → lint → audit → build → test` in sequence. All five must pass (audit skips with a note when `cargo-audit` is not installed).

CI runs the fast gates (`fmt`, `lint`) on every push to `trunk`, and they
fail often — always run `cargo xtask fmt` and `cargo xtask lint` (or
`cargo xtask ci`) **before committing/pushing**. The pre-push hook at
`.githooks/pre-push` enforces this locally; enable it once with
`git config core.hooksPath .githooks`.

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
- RPC integration tests live in `rho/src/rpc.rs` (in the `#[cfg(test)] mod tests` block). These test the JSONL protocol end-to-end using `MockChatClient` via `TestProvider`, with `App` constructed directly to bypass CLI startup. They must live inside the `rho` crate because `App`'s fields are `pub(crate)`.
- `rho-test-helpers` provides `MockChatClient`, `MockShellExecutor`, `TestProvider`, response builders (`text_events`, `tool_call_events`, `multi_tool_call_events`), approval gates (`AutoApproveGate`, `AutoDenyGate`), file-system test environment (`FileTestEnv`), shell detection (`detect_shell`), sandbox helpers (`tempdir_with_sandbox`), and trust-store helpers (`empty_trust_store`).
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
