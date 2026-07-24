# Configuration

rho reads configuration from two TOML files, merged with project-level overrides taking precedence over user-level defaults.

## File locations

| Tier | Path | Purpose |
|---|---|---|
| User-level | `~/.rho/config.toml` | Global defaults: model, providers, approval policies |
| Project-level | `.rho/config.toml` (relative to sandbox root) | Per-project: model, approval policies, command denylist, sandbox, context files |

Both files are optional. Missing files are silently skipped; all fields have sensible defaults.

## Merging strategy

Project-level config overrides user-level config on a per-field basis. For struct fields: if the project sets a field, it wins; if not, the user-level value applies; if neither sets it, the hardcoded default applies.

For `Vec` fields (denylist commands, context scan list, custom redaction patterns): the project-level list **replaces** the user-level list. It does not append. This avoids surprising composition effects.

### Provider merging

Project-level `[[providers]]` **merge** with user-level `[[providers]]` by name (not replaced). This lets you configure providers once globally and override selectively per project:

- **Same name** → project provider overrides user provider entirely (all fields from project)
- **New name** → project provider is appended
- **No match** → user provider preserved in original order
- **No project providers** → user providers preserved unchanged

## Provider presets

The `preset` field auto-fills `endpoint`, `name`, and request `api` from a built-in registry of known providers. This reduces config boilerplate and eliminates typo-prone URLs.

```toml
# Instead of writing out the full endpoint URL:
[[providers]]
preset = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
```

Built-in presets:

| Preset | Endpoint | Protocol | API key env | Notes |
|---|---|---|---|---|
| `lm-studio` | `http://localhost:1234/v1/chat/completions` | Chat Completions | *(none)* | Local, no key needed |
| `ollama` | `http://localhost:11434/v1/chat/completions` | Chat Completions | *(none)* | Local, no key needed |
| `openrouter` | `https://openrouter.ai/api/v1/chat/completions` | Chat Completions | `OPENROUTER_API_KEY` | Remote |
| `openai` | `https://api.openai.com/v1/responses` | Responses | `OPENAI_API_KEY` | Native OpenAI reasoning plus tools |
| `groq` | `https://api.groq.com/openai/v1/chat/completions` | Chat Completions | `GROQ_API_KEY` | Remote |
| `zai` | `https://api.z.ai/api/paas/v4/chat/completions` | Chat Completions | `ZAI_API_KEY` | Z.ai **platform**; models endpoint at `/api/v1/models` |
| `zai-coding` | `https://api.z.ai/api/coding/paas/v4/chat/completions` | Chat Completions | `ZAI_API_KEY` | Z.ai **Coding Plan**; models endpoint derived |

Resolution rules:

- If `preset` is set and `endpoint` is also set → `endpoint` wins; the preset's protocol still applies unless `api` is also explicit
- If `preset` is set and `name` is also set → `name` wins
- If `preset` is set and `api` is also set → explicit `api` wins
- If `api` and `preset` are both absent → `chat_completions` is the safe default
- If `preset` is set and `api_key_env` is not set → a hint is logged at startup (not auto-injected)
- If `preset` is set and `models_endpoint` is also not set and the preset provides one → the preset's `models_endpoint` is used
- If `preset` is set and `models_endpoint` is also set → `models_endpoint` wins (preset is informational)
- If `preset` is set and `endpoint` is overridden to a **different** URL than the preset's → the preset's `models_endpoint` is **skipped** (it would target the wrong API), and the models URL is derived from the chat endpoint instead
- Unknown presets → warning logged, treated as if no preset was set

## Full configuration reference

```toml
[agent]
# Model identifier (highest priority; overrides provider and default_model)
model = "qwen3-8b"

# Provider to use at startup (uses that provider's default_model if model not set)
# provider = "openrouter"

# Maximum model-tool-model round trips before the loop fails (default: 32)
max_iterations = 32

# Maximum retry attempts on transient errors (default: 4)
retry_budget = 4

# Base backoff in milliseconds (default: 500)
initial_backoff_ms = 500

# Context window token budget (default: 32768)
token_budget = 32768

# Tokens reserved for the model's completion (output). Sent as max_tokens.
# Defaults to 16384. Increase for reasoning models (e.g. 32768).
completion_reserve = 16384

# Stuck-loop detection: consecutive identical outputs before nudge (default: 3, 0 = off)
stuck_loop_threshold = 3

# Maximum consecutive empty model responses before aborting (default: 5, 0 = unlimited)
# Prevents infinite retry spirals with models that can't produce output.
max_consecutive_empty = 5

# Show full chain-of-thought reasoning in output (default: false)
show_reasoning = false

# Reasoning effort for thinking-capable models (optional)
# Common values: "low", "medium", "high".
# Sent as `reasoning_effort` for Chat Completions. For Responses, sent as
# reasoning = { effort = ..., summary = "auto" }, with summary deltas streamed.
# Use only values supported by the selected model.
# reasoning_effort = "medium"

# Context utilization threshold for auto-compaction (default: 0 = disabled)
# When utilization >= this %, older entries are proactively compacted.
auto_compact_threshold = 0

# Compaction strategy: "mechanical" (default) or "llm" (opt-in)
# When "llm", the model generates narrative notes for compacted entries.
compaction_mode = "mechanical"

# Maximum seconds to wait for the FIRST stream event before aborting and
# retrying (default: 90, 0 = disabled).
# Guards against hung requests where the server (or an upstream proxy) accepts
# the request but never emits a token — otherwise the loop blocks until the
# upstream's own timeout drops the connection, often as a silent empty reply.
# On timeout the request is retried with backoff. Raise this for slow
# reasoning models.
first_token_timeout_secs = 90

# Maximum gap (seconds) allowed between stream events once streaming has
# started before aborting and retrying (default: 60, 0 = disabled).
# A long gap mid-stream indicates a dead connection.
stream_idle_timeout_secs = 60

# ── Providers ────────────────────────────────────────────────────
# Use the [[providers]] array format for single or multiple providers.
# The legacy [provider] single-table format still works but [[providers]]
# is preferred.
#
# Each provider has:
#   name           — display name (shown in /models, consent prompt)
#   preset         — built-in preset (fills endpoint/name automatically)
#   endpoint       — explicit endpoint URL (overrides preset)
#   api             — "chat_completions" or "responses"; missing defaults to
#                     Chat Completions unless a preset supplies a value
#   api_key_env    — env var holding the API key (not auto-injected by presets)
#   default_model  — model to use when selected via agent.provider
#   models_endpoint — explicit models endpoint URL (overrides preset; used when
#                   the provider serves models at a different path prefix)
#   type           — informational label, has no effect on behavior

# Example: LM Studio via preset (local, no API key needed)
[[providers]]
preset = "lm-studio"

# Example: OpenAI Responses via preset
# [[providers]]
# preset = "openai"
# api_key_env = "OPENAI_API_KEY"
#
# Example: OpenRouter Chat Completions via preset
# [[providers]]
# preset = "openrouter"
# api_key_env = "OPENROUTER_API_KEY"

# Example: custom endpoint (preset as label, endpoint overrides)
# [[providers]]
# name = "my-proxy"
# preset = "openrouter"
# endpoint = "https://my-proxy.example.com/v1/chat/completions"
# api_key_env = "MY_PROXY_KEY"

# Example: custom Responses-compatible endpoint (explicit opt-in)
# [[providers]]
# name = "custom-responses"
# endpoint = "https://llm.example.com/v1/responses"
# api = "responses"
# api_key_env = "CUSTOM_API_KEY"
#
# Example: multiple providers (local + remote)
# [[providers]]
# preset = "lm-studio"
#
# [[providers]]
# preset = "openrouter"
# api_key_env = "OPENROUTER_API_KEY"

# Legacy single-provider format (still supported):
# [provider]
# endpoint = "http://localhost:1234/v1/chat/completions"
# api_key_env = "OPENAI_API_KEY"

[approval.per_tool]
# Per-tool approval overrides: "auto", "ask", "deny"
read_file = "auto"
write_file = "ask"
edit_file = "ask"
run_command = "ask"

[shell]
# Additional commands to deny (appended to the built-in denylist)
denied_commands = ["Remove-Item", "Invoke-WebRequest"]

# Additional denied flag combinations (all flags must be present to deny)
denied_flag_combos = [["-Quiet", "-Force"]]

[context]
# Override the default context file scan list
scan_list = ["AGENTS.md", "CLAUDE.md"]

[redaction]
# Secret redaction toggle (default: true)
enabled = true

# Additional regex patterns to redact
custom_patterns = ["my-key-[a-zA-Z0-9]{32}"]

[memory]
# Enable the project-local knowledge base (MemoryTool). Default: false.
# When enabled, rho-memory opens <project-root>/.rho/memory.db (SQLite + FTS5)
# and the `memory` tool becomes available to the model.
enabled = false

[system_prompt]
# Additional prompt fragments appended after the base prompt
extensions = ["Always use Rust idioms."]

[extensions]
# Extension allowlist — only load these extensions (omit to load all)
enabled = ["hello", "crates-search"]

# Extension denylist — never load these extensions
# disabled = ["experimental-thing"]

[extensions.defaults]
# Default permissions for all extensions
network = true           # allow fetch() by default
max_memory_mb = 64       # V8 heap limit per isolate
max_execution_time_s = 30

[extensions.per_extension."rust-docs"]
# Override defaults for a specific extension
max_memory_mb = 128      # needs more for HTML parsing

[extensions.per_extension."dangerous-tool"]
network = false          # deny fetch()
commands = false         # deny rho.runCommand()
```

## API key handling

API keys are **never** stored in plaintext in config files. Instead, config references environment variable names:

```toml
[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
```

The provider reads the key from the environment variable at runtime. If the variable is not set, the key is silently omitted (local endpoints typically don't need one).

## Provider protocols

The OpenAI preset uses native Responses with stateless requests and `store: false`. OpenRouter, Groq, Z.ai, Ollama, LM Studio, and custom unspecified providers retain Chat Completions. Set `api = "responses"` for a custom endpoint only when it implements the Responses request and SSE formats. See [External Providers](./providers.md) for details and limitations.

## CLI overrides

All config values can be overridden by CLI flags. CLI flags take highest priority:

| Config field | CLI flag | Description |
|---|---|---|
| `agent.model` | `--model` | Model identifier |
| `agent.provider` | — | Provider name (uses its `default_model`) |
| `agent.token_budget` | `--token-budget` | Context window token budget |
| `agent.completion_reserve` | -- | Tokens reserved for model's completion (default: 16384) |
| `provider.endpoint` | `--endpoint` | API endpoint URL (does not change `api`) |
| `provider.api` | — | Protocol is config-only |
| `provider.api_key_env` | `--api-key-env` | Env var holding the API key |
| `agent.max_iterations` | `--max-iterations` | Max agent loop iterations |
| System prompt | `--system` | Override the system prompt |
| Compact prompt | `--compact` | Use the minimal system prompt |
| Provider consent | `--accept-external-provider` | Skip consent warning |
| Session resume | `--session` / `-c` | Resume a previous session |