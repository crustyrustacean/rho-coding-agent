# rho

[![CI](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./License.txt)
[![Rust 2024 Edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

A local coding agent written in Rust. rho runs in your terminal, talks to a model on your machine, and uses tools to read files, edit code, and run commands — with your approval at every step.

## Features

- 🔒 **Safety-first** — file sandbox, approval gates, secret redaction, and untrusted-data framing by default
- 💻 **Cross-platform** — runs on Windows, macOS, and Linux
- 🐚 **PowerShell-native** — the shell is PowerShell (via `pwsh`); the model generates PowerShell commands, not bash
- 📂 **Project-aware** — auto-detects project root, loads context files (`AGENTS.md`, `CLAUDE.md`, `.cursorrules`, etc.) with hash-verified trust
- ⚙️ **Configurable** — two-tier TOML config (user-level `~/.rho/config.toml` + project-level `.rho/config.toml`), per-tool approval policies, command denylist, egress allowlist
- 🧠 **Local models** — targets OpenAI-compatible endpoints on localhost (LM Studio, Ollama); external providers are supported with explicit consent

## Quick Start

1. **Start a local model server** (e.g. [LM Studio](https://lmstudio.ai/) or [Ollama](https://ollama.com/)) on `localhost:1234`.

2. **Build and run:**

   ```sh
   cargo run --package rho
   ```

3. **Ask the agent to do something:**

   ```
   User: list the Rust source files in this project and tell me what each does
   ```

   rho will use its tools to read your project, ask for approval before writing files or running commands, and report back.

## CLI Options

```
rho [OPTIONS]

Options:
  -m, --model <MODEL>                    Model identifier (auto-detected if omitted)
  -s, --system <SYSTEM>                  Override the system prompt
      --compact                          Use a compact prompt for small-context models (~100 tokens)
      --root <ROOT>                      Project/sandbox root (auto-detected if omitted)
      --accept-external-provider         Skip consent warning for external endpoints
      --token-budget <TOKEN_BUDGET>      Context window token budget (default: 32768)
```

### REPL Commands

| Command | Action |
|---|---|
| `/clear` | Reset conversation history (keeps system prompt) |
| `/quit` | Exit the agent |

## Configuration

rho loads config from two TOML files, with project-level overrides taking precedence:

| Source | Path | Purpose |
|---|---|---|
| User-level | `~/.rho/config.toml` | Global defaults: default model, API endpoint, egress allowlist |
| Project-level | `.rho/config.toml` | Per-project: model, approval policies, command denylist, sandbox toggle |

Example `.rho/config.toml`:

```toml
[agent]
model = "qwen3-8b"
token_budget = 32768

[provider]
type = "local"
endpoint = "http://localhost:1234/v1/chat/completions"

[approval.per_tool]
write_file = "ask"
run_command = "ask"
edit_file = "ask"

[shell]
denied_commands = ["Stop-Process"]

[sandbox]
enabled = true

[egress]
allowed_hosts = []

[redaction]
enabled = true
```

API keys are **never** stored in config. Reference environment variables instead:

```toml
[provider]
api_key_env = "OPENAI_API_KEY"
```

## Security Model

rho treats model output as untrusted and applies defense-in-depth:

| Layer | What it does |
|---|---|
| **File sandbox** | All file operations are confined to the project root |
| **Approval gate** | Write, edit, and shell commands require your confirmation |
| **Command denylist** | Dangerous commands (`Remove-Item`, `Invoke-WebRequest`, etc.) are blocked by default |
| **Secret redaction** | API keys and tokens in tool output are replaced with `[REDACTED]` |
| **Untrusted-data framing** | File contents are wrapped in `<context>` tags so the model treats them as data, not instructions |
| **Context-file trust** | Project instruction files (`AGENTS.md`, etc.) are hash-verified; changed files require re-confirmation |
| **Egress allowlist** | The agent only contacts `localhost` unless you add external hosts |
| **Provider consent** | Connecting to an external API triggers a warning before any data leaves your machine |

## Architecture

```
┌──────────────────┐
│      rho         │  ← Binary: CLI, REPL, wiring
├──────────────────┤
│    rho-tools     │  ← Built-in tools: files, shell, denylist
├──────────────────┤
│    rho-core      │  ← Agent kernel: loop, types, traits, config
└──────────────────┘
   rho-test-helpers   ← Dev-only: mocks, fixtures, tempdir helpers
```

**Dependency rule:** crates only depend on layers below them. `rho-core` depends on external libraries only. `rho-tools` depends on `rho-core`. The binary assembles everything.

## Development

```sh
cargo xtask ci                # Full CI pipeline (fmt → lint → build → test)
cargo xtask test              # Run all tests
cargo xtask test -- --nocapture  # Run with stdout visible
cargo xtask changelog <ver>   # Generate CHANGELOG.md
```

### Project layout

```
rho/                  # Binary entry point
rho-core/             # Agent kernel (loop, types, traits, config)
rho-tools/            # Built-in tools (files, shell)
rho-test-helpers/     # Shared test utilities (dev-only)
xtask/                # Dev task runner
```

## Contributing

Contributions are welcome! Please follow [conventional commits](https://www.conventionalcommits.org/):

- `feat:` — new features
- `fix:` — bug fixes
- `docs:` — documentation changes
- `refactor:` — code changes that neither fix bugs nor add features
- `test:` — adding or updating tests
- `chore:` — maintenance tasks

Run `cargo xtask ci` before opening a PR.

## License

[MIT](./License.txt)
