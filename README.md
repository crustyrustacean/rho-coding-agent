# rho

[![CI](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./License.txt)
[![Rust 2024 Edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

A local coding agent written in Rust. rho runs in your terminal, talks to a model on your machine, and uses tools to read files, edit code, and run commands — with your approval at every step.

## Features

- 🔒 **Safety-first** — file sandbox, approval gates, secret redaction, and untrusted-data framing by default
- ✏️ **Hashline editing** — content-addressed line references (LINE#HASH:) prevent stale-context corruption in file edits
- 💻 **Cross-platform** — runs on Windows, macOS, and Linux
- 🐚 **PowerShell-native** — the shell is PowerShell (via `pwsh`); the model generates PowerShell commands, not bash
- 📂 **Project-aware** — auto-detects project root, loads context files (`AGENTS.md`, `CLAUDE.md`, `.cursorrules`, etc.) with hash-verified trust
- ⚙️ **Configurable** — two-tier TOML config (user-level `~/.rho/config.toml` + project-level `.rho/config.toml`), per-tool approval policies, command denylist
- 🧠 **Local and remote models** — targets OpenAI-compatible endpoints (LM Studio, Ollama, OpenAI, Groq, OpenRouter, DeepInfra, and more)

## Quick Start

### Using a local model

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

### Using an external provider (e.g. OpenAI)

1. **Set your API key:**

   ```sh
   export OPENAI_API_KEY="sk-..."
   ```

2. **Create `.rho/config.toml` in your project root:**

   ```toml
   [agent]
   model = "gpt-4o"
   token_budget = 131072

   [provider]
   endpoint = "https://api.openai.com/v1/chat/completions"
   api_key_env = "OPENAI_API_KEY"
   ```

3. **Run:**

   ```sh
   cargo run --package rho -- --accept-external-provider
   ```

   The `--accept-external-provider` flag skips the consent prompt that warns your data will be sent to an external server. Omit it on first use to see the warning.

   See [External Providers](docs/src/providers.md) for more providers (Groq, OpenRouter, DeepInfra) and detailed configuration.

   **Note:** rho speaks the OpenAI Chat Completions wire format. Providers with their own API format (Anthropic, Google Gemini, AWS Bedrock) require an [OpenAI-compatible proxy](https://github.com/BerriAI/litellm) like LiteLLM or OpenRouter.

## CLI Options

```
rho [OPTIONS]

Options:
  -m, --model <MODEL>                    Model identifier (auto-detected if omitted)
  -s, --system <SYSTEM>                  Override the system prompt
      --compact                          Use a compact prompt for small-context models (~100 tokens)
      --root <ROOT>                      Project/sandbox root (auto-detected if omitted)
      --endpoint <URL>                    API endpoint URL (overrides config)
      --api-key-env <VAR>                 Environment variable holding the API key
      --max-iterations <N>                Maximum agent loop iterations
      --accept-external-provider         Skip consent warning for external endpoints (also implied by --endpoint)
      --token-budget <TOKEN_BUDGET>      Context window token budget (default: 32768)
      --prompt-file <FILE>               Read a prompt from a file, then exit
      --session <PATH>                   Resume a previous session from a JSONL file
      --ephemeral                        Run without disk persistence
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
| User-level | `~/.rho/config.toml` | Global defaults: default model, API endpoint |
| Project-level | `.rho/config.toml` | Per-project: model, approval policies, command denylist, sandbox toggle |

Example `.rho/config.toml` (local model):

```toml
[agent]
model = "qwen3-8b"
token_budget = 32768

[provider]
endpoint = "http://localhost:1234/v1/chat/completions"

[approval.per_tool]
write_file = "ask"
run_command = "ask"
edit_file = "ask"

[shell]
denied_commands = ["Stop-Process"]

[sandbox]
enabled = true

[redaction]
enabled = true
```

Example `.rho/config.toml` (OpenAI):

```toml
[agent]
model = "gpt-4o"
token_budget = 131072

[provider]
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
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
| **Provider consent** | Connecting to an external API triggers a warning before any data leaves your machine |

## Architecture

```
┌──────────────────┐
│      rho         │  ← Binary: CLI, REPL, wiring
├──────────────────┤
│    rho-tools     │  ← Built-in tools: files, shell, rust tooling
├──────────────────┤
│  rho-highlight   │  ← Tree-sitter syntax analysis (node splitting, highlighting)
├──────────────────┤
│    rho-core      │  ← Agent kernel: loop, types, traits, config
└──────────────────┘
   rho-test-helpers   ← Dev-only: mocks, fixtures, tempdir helpers
   rho-eval            ← Dev-only: behavioural benchmark suite (10 eval tasks)
   rho-bench          ← Dev-only: benchmark harness (multi-model comparison)
```

**Dependency rule:** crates only depend on layers below them. `rho-core` depends on external libraries only. `rho-tools` and `rho-highlight` depend on `rho-core`. The binary assembles everything. `rho-bench` depends on `rho-eval` → `rho-core` + `rho-tools`.

## Development

```sh
cargo xtask ci                # Full CI pipeline (fmt → lint → build → test)
cargo xtask test              # Run all tests
cargo xtask test -- --nocapture  # Run with stdout visible
cargo xtask changelog <ver>   # Generate CHANGELOG.md
```

### Project layout

```
rho/                  # Binary entry point + library crate
  src/
    main.rs           # Thin: parse CLI, build App, run
    lib.rs            # Module declarations
    cli.rs            # CLI argument parsing (17 flags)
    app.rs            # App struct — startup orchestration
    gate.rs           # REPL approval gate (replaced by TUI in Phase 4)
    repl.rs           # REPL loop and prompt-file mode
rho-core/             # Agent kernel (loop, types, traits, config)
rho-tools/            # Built-in tools (files, shell, rust tooling)
rho-highlight/        # Tree-sitter syntax analysis
rho-eval/             # Behavioural benchmark suite (10 eval tasks)
rho-bench/            # Benchmark harness (multi-model comparison)
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
