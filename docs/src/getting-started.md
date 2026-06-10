# Getting Started

## Prerequisites

- Rust toolchain (edition 2024)
- [PowerShell 7+](https://learn.microsoft.com/en-us/powershell/scripting/install/installing-powershell) (`pwsh`) — required for shell command execution
- A local LLM server (LM Studio, Ollama) running on `localhost:1234`, **or** an API key for an external provider (OpenAI, Groq, OpenRouter, DeepInfra, etc.)

### Local model (default)

No configuration needed — just start a local server on `localhost:1234` and run rho. It will auto-detect the loaded model.

### External provider

Create `~/.rho/config.toml` with a provider preset. See [External Providers](./providers.md) for worked examples.

```toml
[[providers]]
preset = "openrouter"
api_key_env = "OPENROUTER_API_KEY"

[agent]
model = "deepseek-v4-flash"
```

### No server running?

If you run rho with no local server and no configured external provider, rho will print a startup error indicating no models were found.

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
cargo build --release -p rho        # headless RPC agent
```

## Run

```sh
rho                      # auto-detect model, persist session to disk (headless RPC)
rho -m my-model          # specify a model
rho -c                   # resume the most recent session for this project
rho --session <path>     # resume a specific session
rho --ephemeral          # in-memory mode, no session file
```

rho runs as a headless JSON-RPC 2.0 agent over stdin/stdout. Send prompts and receive streaming responses via the protocol. See [RPC Mode](./rpc-mode.md) for the full protocol reference.

Example scripted use:

```sh
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"fix the bug"},"id":1}' | rho --model <id>
```

On startup, rho:

1. Auto-detects the project root by walking up from the current directory looking for markers.
2. Loads configuration from `~/.rho/config.toml` and `.rho/config.toml`.
3. Scans for project context files (`AGENTS.md`, etc.) and prompts for trust on first encounter.
4. Checks provider consent for external endpoints.
5. Connects to the model server and auto-detects the loaded model (unless `--model` is specified). If no model can be found, rho returns an error.
6. Creates a session (persisted to `~/.rho/sessions/` by default, or in-memory with `--ephemeral`). If previous sessions exist, a hint is printed.
7. Enters the RPC loop and waits for commands.

## CLI flags (`rho`)

The `rho` binary runs in headless JSON-RPC 2.0 mode.

| Flag | Description |
|---|---|
| Flag | Description |
|---|---|
| `-m, --model <MODEL>` | Model identifier (auto-detected if omitted) |
| `-s, --system <SYSTEM>` | Override the system prompt |
| `--compact` | Use a minimal system prompt for small-context models |
| `--root <ROOT>` | Project root / sandbox root |
| `--token-budget <N>` | Context window token budget (default: 32768) |
| `--endpoint <URL>` | API endpoint URL (overrides config) |
| `--api-key-env <VAR>` | Environment variable holding the API key |
| `--max-iterations <N>` | Maximum agent loop iterations |
| `--accept-external-provider` | Skip consent prompt for non-local providers |
| `-c, --continue` | Resume the most recent session for this project |
| `--session <PATH>` | Resume a specific session from a JSONL file |
| `--ephemeral` | Run without disk persistence |
## Session persistence

By default, every rho invocation creates a new JSONL session file under `~/.rho/sessions/<project-hash>/`. Sessions survive process restarts — use `rho -c` to resume the latest session or `--session <path>` to resume a specific one.

The `listSessions` RPC method returns the 10 most recent sessions with timestamps, sizes, and entry counts. The latest session is marked so you know which one `rho -c` will pick.

See [Sessions](./core-concepts/sessions.md) for details.

## RPC methods

rho communicates via JSON-RPC 2.0 over stdin/stdout. See [RPC Mode](./rpc-mode.md) for the full protocol reference, including all methods, notifications, and the approval flow.

## Next Steps

- [Architecture](./architecture.md) — how the crates fit together
- [Core Concepts](./core-concepts.md) — sessions, agent loop, context management
- [Security](./security.md) — the threat model and defenses
- [Configuration](./configuration.md) — customizing behavior
- [RPC Mode](./rpc-mode.md) — headless JSONL integration