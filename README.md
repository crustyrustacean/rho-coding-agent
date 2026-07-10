# rho

[![CI](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml/badge.svg)](https://github.com/crustyrustacean/rho-coding-agent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./License.txt)
[![Rust 2024 Edition](https://img.shields.io/badge/edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

A local coding agent written in Rust. `rho` runs as a headless process communicating via JSON-RPC 2.0 over stdin/stdout, talks to a model on your machine, and uses tools to read files, edit code, and run commands — with your approval at every step.

## Features

- 🔒 **Safety-first** — file sandbox, approval gates, secret redaction, and untrusted-data framing by default
- ✏️ **Hashline editing** — content-addressed line references (LINE#HASH:) prevent stale-context corruption in file edits
- 💻 **Cross-platform** — runs on Windows, macOS, and Linux
- 🐚 **PowerShell-native** — the shell is PowerShell (via `pwsh`); the model generates PowerShell commands, not bash
- 📂 **Project-aware** — auto-detects project root, loads context files (`AGENTS.md`, `CLAUDE.md`, `.cursorrules`, etc.) with hash-verified trust
- ⚙️ **Configurable** — two-tier TOML config (user-level `~/.rho/config.toml` + project-level `.rho/config.toml`), per-tool approval policies, command denylist
- 🧠 **Local and remote models** — targets OpenAI-compatible endpoints (LM Studio, Ollama, OpenAI, Groq, OpenRouter, DeepInfra, and more) with named presets (`lm-studio`, `openrouter`, `openai`, `groq`, `ollama`, `zai`) and multi-provider support
- 🔌 **JSON-RPC 2.0** — headless protocol over stdin/stdout for embedding in editors, bots, and custom UIs. [OpenRPC schema](docs/rpc-schema/openrpc.json) available for client generation.

## Quick Start

### Prerequisites

- Rust toolchain (edition 2024)
- [PowerShell 7+](https://learn.microsoft.com/en-us/powershell/scripting/install/installing-powershell) (`pwsh`) — required for shell command execution
- A local model server (e.g. [LM Studio](https://lmstudio.ai/) or [Ollama](https://ollama.com/)) on `localhost:1234`, **or** an API key for an external provider

### Using a local model

1. **Start a local model server** on `localhost:1234`.

2. **Build and run:**

   ```sh
   cargo run --package rho --model <model-id>
   ```

   Or send prompts via JSON-RPC:

   ```sh
   echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"list the source files"},"id":1}' | \
     cargo run --package rho --model <model-id>
   ```

   rho will use its tools to read your project, emit streaming events as notifications, and return a response via JSON-RPC.

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

   [[providers]]
   preset = "openai"
   api_key_env = "OPENAI_API_KEY"
   ```

3. **Run:**

   ```sh
   cargo run --package rho -- --accept-external-provider
   ```

   The `--accept-external-provider` flag skips the consent warning that warns your data will be sent to an external server. Omit it on first use to see the warning.

   See [External Providers](docs/src/providers.md) for more providers (Groq, OpenRouter, DeepInfra) and detailed configuration.

   **Note:** rho speaks the OpenAI Chat Completions wire format. Providers with their own API format (Anthropic, Google Gemini, AWS Bedrock) require an [OpenAI-compatible proxy](https://github.com/BerriAI/litellm) like LiteLLM or OpenRouter.

## CLI Options

```
rho [OPTIONS]

Options:
  -m, --model <MODEL>                    Model identifier (uses config default if omitted)
  -s, --system <SYSTEM>                  Override the system prompt
      --compact                          Use a compact prompt for small-context models (~100 tokens)
      --root <ROOT>                      Project/sandbox root (auto-detected if omitted)
      --endpoint <URL>                    API endpoint URL (overrides config)
      --api-key-env <VAR>                 Environment variable holding the API key
      --max-iterations <N>                Maximum agent loop iterations
      --accept-external-provider         Skip consent warning for external endpoints (also implied by --endpoint)
      --token-budget <TOKEN_BUDGET>      Context window token budget (default: 32768)
      --session <PATH>                   Resume a previous session from a JSONL file
  -c, --continue                         Resume the most recent session for this project
      --ephemeral                        Run without disk persistence
```

## JSON-RPC 2.0 Protocol

rho communicates via [JSON-RPC 2.0](https://www.jsonrpc.org/specification) over stdin/stdout. Diagnostic output (warnings, budget info) goes to stderr.

### Example

```sh
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"list the source files"},"id":1}' | \
  cargo run --package rho -- \
    --ephemeral \
    --endpoint http://localhost:1234/v1/chat/completions \
    --model my-model
```

Output is a stream of JSON-RPC responses and notifications (`ready`, `agent/start`, `message/delta`, `tool/call`, `tool/result`, `agent/end`, etc.). See [ARCHITECTURE.md](ARCHITECTURE.md) for the full protocol reference, or [docs/rpc-schema/openrpc.json](docs/rpc-schema/openrpc.json) for the machine-readable `OpenRPC` schema.

## Configuration

rho loads config from two TOML files, with project-level overrides taking precedence:

| Source | Path | Purpose |
|---|---|---|
| User-level | `~/.rho/config.toml` | Global defaults: default model, API endpoint |
| Project-level | `.rho/config.toml` | Per-project: model, approval policies, command denylist, sandbox toggle |

Example `.rho/config.toml` (local model with preset):

```toml
[agent]
model = "qwen3-8b"
token_budget = 32768

[[providers]]
preset = "lm-studio"

[approval.per_tool]
write_file = "ask"
run_command = "ask"
edit_file = "ask"

[shell]
denied_commands = ["Stop-Process"]

[redaction]
enabled = true
```

Example `.rho/config.toml` (OpenAI with preset):

```toml
[agent]
model = "gpt-4o"
token_budget = 131072

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
```

API keys are **never** stored in config. Reference environment variables instead:

```toml
[[providers]]
preset = "openai"
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
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │  ← Headless JSON-RPC 2.0 agent
├─────────────────────────────────────────────────┤
│  rho-ext          rho-tools                       │  ← Extensions   /  Built-in tools
├─────────────────────────────────────────────────┤
│            rho-memory      rho-highlight           │  ← Siblings of rho-tools
├─────────────────────────────────────────────────┤
│                   rho-core                       │  ← Agent kernel: loop, types, traits, config
├─────────────────────────────────────────────────┤
│                   rho-ai                         │  ← Unified LLM provider abstraction (streaming, retry, SSE)
└─────────────────────────────────────────────────┘
   rho-test-helpers   ← Dev-only: mocks, fixtures, tempdir helpers
```

**Dependency rule:** crates only depend on layers below them. `rho-ai` is the lowest layer; `rho-core` depends on it for the `LlmService` trait. `rho-highlight` and `rho-memory` depend on `rho-core`. `rho-tools` depends on `rho-core`, `rho-highlight`, and `rho-memory`. `rho-ext` depends on `rho-core` for trait implementations. The binary assembles everything.

## Development

```sh
cargo xtask ci                # Full CI pipeline (fmt → lint → build → test)
cargo xtask test              # Run all tests
cargo xtask test -- --nocapture  # Run with stdout visible
cargo xtask changelog <ver>   # Generate CHANGELOG.md
cargo xtask schema             # Update version in OpenRPC schema
cargo xtask generate-models    # Regenerate the rho-ai model catalog from OpenRouter
cargo xtask fmt-fix            # Auto-format the workspace (cargo fmt --all)
```

### Pre-push hook

The fast gates (`cargo xtask fmt`, `cargo xtask lint`) run in CI on every
push to `trunk`. To catch them locally *before* a push — rather than as a
red check after the fact — install the pre-push hook:

```sh
git config core.hooksPath .githooks
```

This runs `fmt` and `lint` (the exact same commands CI uses) and blocks the
push on failure. Bypass once with `git push --no-verify`. The hook skips the
slow build/test steps; run `cargo xtask ci` for the full local pipeline.

### Project layout

```
rho/                  # Headless JSON-RPC 2.0 agent
  src/
    main.rs           # Thin: parse CLI, build App, run
    lib.rs            # Module declarations
    cli.rs            # CLI argument parsing
    app.rs            # App struct — startup orchestration, extension loading, AgentResult extraction
    model.rs          # Model resolution
    ext_observer.rs   # CompositeObserver — fans out to RPC + extension observers
    rpc.rs            # JSON-RPC 2.0 protocol (methods, notifications, approval gate)
    rpc_wire.rs        # Typed wire-format structs (params, results, notifications)
    transport.rs       # Transport trait (StdioTransport)
    presenter.rs      # Presenter module root
    presenter/
      rpc.rs          # RpcPresenter — diagnostic output to stderr
rho-ai/               # Unified LLM provider abstraction (streaming, retry, SSE)
rho-ext/              # TypeScript extension runtime (V8/deno-core)
rho-core/             # Agent kernel (loop, types, traits, config)
rho-tools/            # Built-in tools (files, shell, rust tooling, memory)
rho-memory/           # Persistent knowledge base (SQLite/FTS5)
rho-highlight/        # Tree-sitter syntax analysis
rho-test-helpers/     # Shared test utilities (dev-only)
xtask/                # Dev task runner
```

## Community Discord - Coming Soon

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