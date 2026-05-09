# Project Context Files

rho scans for project-level instruction files and injects them into the system prompt. This lets projects tell the agent how to work with their codebase — without modifying rho itself.

## Scan list

rho looks for the following files in the project root (sandbox root), in priority order:

| File | Purpose |
|---|---|
| `AGENTS.md` | Instructions for AI assistants (rho-specific) |
| `.agents.md` | Hidden variant of `AGENTS.md` |
| `CLAUDE.md` | Instructions for Claude-compatible agents |
| `.cursorrules` | Cursor editor rules |
| `.rho/prompt.md` | rho-specific project prompt |

The scan list can be overridden in config:

```toml
[context]
scan_list = ["AGENTS.md", "CLAUDE.md"]
```

## Trust model

Project context files are a prompt injection vector — anyone with write access to the repository can modify them. rho addresses this with a trust store:

1. **First encounter:** rho prompts the user to review the file contents and confirm trust.
2. **Trust stored:** the SHA-256 hash of the accepted file is stored in `~/.rho/trusted_projects.toml`.
3. **Subsequent loads:** if the hash matches, the file is loaded silently. If it changed since the last trust decision, rho prompts the user again.

This ensures:
- A malicious PR that silently modifies `AGENTS.md` will trigger a re-review prompt.
- Legitimate updates (new project instructions) are confirmed once, then loaded automatically.
- The user can always audit what's in `trusted_projects.toml`.

## Prompt composition

Context files are appended to the system prompt after the base prompt, in scan-list order. The final system prompt structure is:

```text
[base prompt]           ← prompts/base.md (or compact prompt)
[project context files] ← AGENTS.md, CLAUDE.md, etc.
[environment block]     ← working directory, shell info
[Rust tooling block]    ← cargo check/clippy/test guidance
```

## In practice

The file you're reading right now (`AGENTS.md`) is a context file. It tells rho about the project layout, conventions, testing approach, and commit format. Without it, rho would still work — but it wouldn't know about `cargo xtask ci`, the conventional commit format, or the `forbid(unsafe_code)` lint.
