# Getting Started

## Prerequisites

- Rust toolchain (edition 2024)
- A local LLM server (LM Studio, Ollama) running on `localhost:1234`, **or** an API key for an external provider (OpenAI, Groq, OpenRouter, DeepInfra, etc.)

### Local model (default)

No configuration needed — just start a local server on `localhost:1234` and run `rho`. It will auto-detect the loaded model.

### External provider

Create `~/.rho/config.toml` with the endpoint and API key env var. See [External Providers](./providers.md) for worked examples.

### No server running?

If you run `rho` with no local server and no configured external provider, you'll see a getting-started guide with setup instructions.

If you have an external provider configured but rho cannot list models (e.g. the API key is wrong or the endpoint is slow), rho offers an interactive model picker:

```text
  Select a model to use:

    [1] Claude Sonnet 4        (Anthropic)
    [2] GPT-4o                 (OpenAI)
    [3] GLM-5                  (z.ai)
    [0] Enter model ID manually

  Choice:
```

You can also skip the picker entirely with `--model <id>`.

## Build

```sh
cargo build --release -p rho
```

## Run

```sh
rho                    # auto-detect model, persist session to disk
rho -m my-model       # specify a model
rho --ephemeral       # in-memory mode, no session file
```

On startup, rho:

1. Auto-detects the project root by walking up from the current directory looking for markers.
2. Loads configuration from `~/.rho/config.toml` and `.rho/config.toml`.
3. Scans for project context files (`AGENTS.md`, etc.) and prompts for trust on first encounter.
4. Checks provider consent for external endpoints.
5. Connects to the model server and auto-detects the loaded model (unless `--model` is specified). If no model can be found, rho offers an interactive model picker.
5. Creates a session (persisted to `~/.rho/sessions/` by default, or in-memory with `--ephemeral`).
6. Enters the REPL. Type your request, and the agent loop runs until the model produces a final text reply.

## CLI flags

| Flag | Description |
|---|---|
| `-m, --model <MODEL>` | Model identifier (auto-detected if omitted) |
| `-s, --system <SYSTEM>` | Override the system prompt |
| `--compact` | Use a minimal system prompt for small-context models |
| `--root <ROOT>` | Project root / sandbox root |
| `--token-budget <N>` | Context window token budget (default: 32768) |
| `--prompt-file <FILE>` | Read a prompt from a file, then exit |
| `--session <PATH>` | Resume a previous session from a JSONL file |
| `--ephemeral` | Run without disk persistence |
| `--accept-external-provider` | Skip consent prompt for non-local providers |

## Session persistence

By default, every rho invocation creates a new JSONL session file under `~/.rho/sessions/<project-hash>/`. Sessions survive process restarts — use `--session <path>` to resume one.

See [Sessions](./core-concepts/sessions.md) for details.

## REPL commands

| Command | Description |
|---|---|
| `/clear` | Branch back to the system message (preserves old tree on disk) |
| `/models` | List all models across all providers |
| `/model <id>` | Switch to a model (fuzzy match or `provider/model` syntax) |
| `/quit` | Exit rho |

## Next Steps

- [Architecture](./architecture.md) — how the crates fit together
- [Core Concepts](./core-concepts.md) — sessions, agent loop, context management
- [Security](./security.md) — the threat model and defenses
- [Configuration](./configuration.md) — customizing behavior
