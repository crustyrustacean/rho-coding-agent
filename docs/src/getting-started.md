# Getting Started

## Prerequisites

- Rust toolchain (edition 2024)
- [PowerShell 7+](https://learn.microsoft.com/en-us/powershell/scripting/install/installing-powershell) (`pwsh`) — required for shell command execution
- A local LLM server (LM Studio, Ollama) running on `localhost:1234`, **or** an API key for an external provider (OpenAI, Groq, OpenRouter, DeepInfra, etc.)

### Local model (default)

No configuration needed — just start a local server on `localhost:1234` and run `rho-repl`. It will auto-detect the loaded model.

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

If you run `rho-repl` with no local server and no configured external provider, rho will print a startup error indicating no models were found.

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
cargo build --release -p rho-repl   # interactive terminal client
```

## Run

```sh
rho-repl                # auto-detect model, persist session to disk
rho-repl -m my-model    # specify a model
rho-repl -c             # resume the most recent session for this project
rho-repl --session <path>  # resume a specific session
rho-repl --ephemeral   # in-memory mode, no session file
```

`rho-repl` spawns `rho` as a subprocess and communicates via JSON-RPC 2.0 over stdin/stdout. All model interaction happens inside `rho`; `rho-repl` provides the terminal UI, slash commands, and approval prompts.

You can also run `rho` directly for headless/scripted use:

```sh
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"fix the bug"},"id":1}' | rho --model <id>
```

On startup, rho:

1. Auto-detects the project root by walking up from the current directory looking for markers.
2. Loads configuration from `~/.rho/config.toml` and `.rho/config.toml`.
3. Scans for project context files (`AGENTS.md`, etc.) and prompts for trust on first encounter.
4. Checks provider consent for external endpoints.
5. Connects to the model server and auto-detects the loaded model (unless `--model` is specified). If no model can be found, rho offers an interactive model picker.
6. Creates a session (persisted to `~/.rho/sessions/` by default, or in-memory with `--ephemeral`). If previous sessions exist, a hint is printed.
7. Enters the RPC loop and waits for commands. `rho-repl` provides the interactive prompt.

## CLI flags (`rho`)

The `rho` binary runs in headless JSON-RPC 2.0 mode. All flags can also be forwarded by `rho-repl`.

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

By default, every rho invocation creates a new JSONL session file under `~/.rho/sessions/<project-hash>/`. Sessions survive process restarts — use `rho-repl -c` to resume the latest session or `--session <path>` to resume a specific one.

The `/sessions` REPL command lists the 10 most recent sessions with timestamps, sizes, and entry counts. The latest session is marked so you know which one `rho -c` will pick.

See [Sessions](./core-concepts/sessions.md) for details.

## REPL commands (`rho-repl`)

| Command | Description |
|---|---|
| `/help` | Show available commands |
| `/clear` | Clear conversation history (creates a new branch) |
| `/models` | List all models across all providers |
| `/model <id>` | Switch model (bare id or `provider:id`) |
| `/providers` | List configured providers with reachability status |
| `/status` | Show context window usage breakdown |
| `/sessions` | List previous sessions for this project |
| `/extensions` | List loaded extension names |
| `/reload` | Hot-reload TypeScript extensions from disk |
| `/compact` | Trigger context compaction |
| `/quit` or `/exit` | Exit rho-repl |

## Next Steps

- [Architecture](./architecture.md) — how the crates fit together
- [Core Concepts](./core-concepts.md) — sessions, agent loop, context management
- [Security](./security.md) — the threat model and defenses
- [Configuration](./configuration.md) — customizing behavior
- [RPC Mode](./rpc-mode.md) — headless JSONL integration