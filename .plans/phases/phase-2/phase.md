# Phase 2: PowerShell, File Tools, and Cross-Platform Support

**Goal:** The agent is a credible PowerShell-native assistant that can navigate and manipulate the file system, running on Windows, macOS, and Linux.

**Milestone:** The agent can be asked "list the Rust source files in this project and tell me what each does" and it works.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `toml` **(foundation)** | `rho-core` | TOML parsing for config files — the format is non-trivial to parse correctly, and `toml` is the standard Rust crate |
| `ignore` **(foundation)** | `rho-tools` | `.gitignore`-aware directory walking from the ripgrep author — handling negation, nested files, and the full spec correctly would be ~200+ lines of subtle code |
| `regex` **(foundation)** | `rho-core` | Custom redaction patterns — the standard Rust regex library |
| `which` | `rho-tools` | `pwsh`/`powershell` detection at runtime — pure lookup without process spawning |

## Exit Criteria

The agent reliably uses PowerShell commands, reads and edits files, and the model generates syntactically valid PowerShell. The agent loop handles multiple tool calls in a single model response. Shell execution is abstracted behind `ShellExecutor`. Minimal config loading works (model, system prompt, per-tool approval policies, command denylist, egress allowlist). The token budget is configurable via config and CLI flag, with a default (32K) that leaves sufficient room for multi-turn conversation after the system prompt. Path handling and process management are platform-aware (Windows, macOS, Linux). The project root and model are auto-detected when not specified. A compact system prompt is available for small-context-window models. HTTP errors preserve status codes for retry classification and provide actionable diagnostics.

**Security:** dangerous commands are blocked by default, secrets are redacted from tool results, API keys are read from env vars (never plaintext), and external provider use requires explicit consent.
