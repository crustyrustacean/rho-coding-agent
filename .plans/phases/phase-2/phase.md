# Phase 2: PowerShell and File Tools

**Goal:** The agent is a credible PowerShell-native assistant that can navigate and manipulate the file system.

**Milestone:** The agent can be asked "list the Rust source files in this project and tell me what each does" and it works.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `toml` **(foundation)** | `rho-core` | TOML parsing for config files — the format is non-trivial to parse correctly, and `toml` is the standard Rust crate |

`RunCommand` shells out to `pwsh`/`powershell` via `tokio::process::Command` (already in `tokio`). `EditFile` does string matching in pure Rust. `ListDir` uses `std::fs` and walks `.gitignore` rules — the ignore logic is the one place we'd consider a crate, but `.gitignore` matching is well-understood and can be written in ~200 lines. If it proves painful, `ignore` (the ripgrep crate) would be the fallback. Config loading uses `toml` (new dependency).

## Exit Criteria

The agent reliably uses PowerShell commands, reads and edits files, and the model generates syntactically valid PowerShell. The agent loop handles multiple tool calls in a single model response. Shell execution is abstracted behind `ShellExecutor`. Minimal config loading works (model, system prompt, per-tool approval policies, command denylist, egress allowlist). The token budget is configurable via config and CLI flag, with a default (32K) that leaves sufficient room for multi-turn conversation after the system prompt.

**Security:** dangerous commands are blocked by default, secrets are redacted from tool results, API keys are read from env vars (never plaintext), and external provider use requires explicit consent.
