# External Providers

rho supports two OpenAI-shaped streaming protocols behind the same internal `LlmService` contract:

- **Responses** (`POST /v1/responses`) through `ResponsesService`, used by the built-in `openai` preset. This supports OpenAI reasoning and function tools in the same turn.
- **Chat Completions** (`POST /v1/chat/completions`) through `OpenAiService`, used by every other built-in preset and by custom providers unless explicitly overridden.

The agent loop, tools, sessions, and UIs receive the same text, reasoning, tool-call, usage, and completion events from either transport.

## Chat Completions compatibility

A Chat Completions request uses `messages` and nested function tools:

```json
{
  "model": "gpt-4o",
  "messages": [...],
  "tools": [...]
}
```

Any endpoint that implements the OpenAI Chat Completions request, SSE, tool-call, and usage shapes can be used. This remains the safe default for unspecified custom providers.

## Native OpenAI Responses

A Responses request uses `input`, `instructions`, flat function tools, and `max_output_tokens`. rho operates statelessly: it sends the fitted conversation on every turn and explicitly sets `store: false`; it does not use `previous_response_id` or server-side Conversations. When `reasoning_effort` is configured, rho requests an automatic reasoning summary and streams summary deltas through the existing reasoning event path.

Function calls and outputs round-trip as `function_call` and `function_call_output` items correlated by `call_id`. Encrypted reasoning items and assistant `phase` metadata are not persisted in this first implementation, so full hidden-reasoning continuity for zero-data-retention/Codex-style workflows is deferred.

### Supported providers and defaults

| Provider | Endpoint | Default protocol | Notes |
|---|---|---|---|
| OpenAI preset | `api.openai.com/v1/responses` | Responses | Native reasoning plus function calls |
| OpenRouter | `openrouter.ai/api/v1/chat/completions` | Chat Completions | Proxies many model providers |
| Groq | `api.groq.com/openai/v1/chat/completions` | Chat Completions | OpenAI-compatible path |
| DeepInfra | `api.deepinfra.com/v1/openai/chat/completions` | Chat Completions | Custom configuration |
| LM Studio | `localhost:1234/v1/chat/completions` | Chat Completions | Local server |
| Ollama | `localhost:11434/v1/chat/completions` | Chat Completions | Local server |
| Together AI | `api.together.xyz/v1/chat/completions` | Chat Completions | Custom configuration |
| Fireworks | `api.fireworks.ai/inference/v1/chat/completions` | Chat Completions | Custom configuration |
| Z.ai | `api.z.ai/api/paas/v4/chat/completions` | Chat Completions | GLM models |

OpenRouter, Groq, Z.ai, Ollama, LM Studio, and unspecified custom endpoints do **not** switch to Responses as a side effect of this feature.

### Providers with unrelated native formats

Native Anthropic Messages, Google GenerateContent, AWS Bedrock, Cohere, and other unrelated formats are not implemented directly. Use an OpenAI-compatible proxy such as LiteLLM or OpenRouter if it exposes a tested Chat Completions interface.

## Prerequisites

To use an external provider you need two things:

1. **An API key** from the provider
2. **The provider's endpoint URL** (matching the configured `api` protocol)

That's it. Configure via config file or CLI flags — see below.

## Quick start: OpenAI

### Via CLI flags

A CLI-only endpoint uses the backward-compatible Chat Completions default:

```sh
export OPENAI_API_KEY="sk-..."

rho --endpoint https://api.openai.com/v1/chat/completions \
    --api-key-env OPENAI_API_KEY \
    --model gpt-4o
```

`--endpoint` implies consent. There is no separate `--api` flag; use provider config below to select Responses.

### Via config

```toml
# ~/.rho/config.toml
[agent]
model = "gpt-5"
provider = "openai"
token_budget = 131072
reasoning_effort = "medium"

[[providers]]
preset = "openai" # supplies api = "responses" and /v1/responses
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-5"
```

```sh
export OPENAI_API_KEY="sk-..."
rho --accept-external-provider
```

## Provider consent

When connecting to a non-local endpoint, rho checks for external providers before starting. If external providers are configured and neither `--accept-external-provider` nor `--endpoint` is passed, rho prints a warning to stderr and exits with an error:

```text
  ⚠  No local model server detected
  ⚠  External provider(s) configured:
      - openrouter

      Your prompts and code will be sent to external servers.
      This may expose proprietary code, secrets, or other
      sensitive data to the providers and any intermediaries.

  Aborting. Use --accept-external-provider to skip this prompt.
```

If rho also has a local provider (e.g. LM Studio is running alongside an external provider), the "No local model server detected" header is omitted.

The consent gate fires automatically for config-driven external endpoints. It is **skipped** when you use `--endpoint` on the CLI (explicit endpoint implies consent) or `--accept-external-provider`.

## Specifying the model

There are several ways to set the model, in priority order:

1. **CLI flag** — `--model gpt-4o` (highest priority)
2. **Config model** — `[agent] model = "gpt-4o"`
3. **Config provider** — `[agent] provider = "openrouter"` uses that provider's `default_model`
4. **Provider default** — the first provider's `default_model` if configured
5. **CLI flags** — `--endpoint <url> --model <id>` (no config needed)

rho does **not** call `/v1/models` at startup. The config file is the source of truth — model names are accepted as-is, and misconfiguration surfaces as a clear HTTP error at request time.

### `listProviders` RPC method

The `listProviders` RPC method shows all configured providers with their reachability status:

```text
  configured providers:
  * lm-studio (local, ok)
    openrouter (remote, ok)
    ollama (remote, down)
  * = active
```

Each entry shows:

- **`*`** — active provider (where the next prompt will go)
- **local/remote** — whether the endpoint is on localhost
- **ok/down** — whether the provider's models endpoint responded

### Mid-session provider switching

The `/model` command accepts `provider:model` syntax to switch to a specific provider without model discovery. This is useful when a provider is slow to respond or its models endpoint is unavailable:

```
/model openrouter:deepseek/deepseek-v4-flash
```

With a bare model ID, rho discovers which provider serves it by querying each provider's models endpoint and switches automatically:

```
/model qwen3-8b
```

## CLI flags for provider configuration

| Flag | Config override | Description |
|---|---|---|
| `--endpoint <URL>` | `provider.endpoint` | API endpoint URL |
| `--api-key-env <VAR>` | `provider.api_key_env` | Environment variable holding the API key |
| `--model <MODEL>` | `agent.model` | Model identifier |
| `--accept-external-provider` | — | Skip consent prompt for non-local endpoints |

Priority: CLI flag → project config → user config → hardcoded default.

## Provider examples

### OpenAI

Use the preset for native Responses, including reasoning plus function tools:

```toml
[agent]
model = "gpt-5"
provider = "openai"
reasoning_effort = "medium"

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-5"
```

To force OpenAI Chat Completions for an older workflow, override both fields explicitly:

```toml
[[providers]]
name = "openai-chat"
endpoint = "https://api.openai.com/v1/chat/completions"
api = "chat_completions"
api_key_env = "OPENAI_API_KEY"
```

### Custom Responses-compatible endpoint

Responses is opt-in for custom servers and proxies:

```toml
[[providers]]
name = "custom-responses"
endpoint = "https://llm.example.com/v1/responses"
api = "responses"
api_key_env = "CUSTOM_API_KEY"
```

Only select this when the endpoint implements OpenAI's Responses request items and SSE event taxonomy. Merely accepting Chat Completions is not sufficient.

### OpenRouter

OpenRouter proxies many models behind a single endpoint. Model names include the provider prefix (e.g. `anthropic/claude-sonnet-4-20250514`).

```sh
export OPENROUTER_API_KEY="sk-or-..."
rho --endpoint https://openrouter.ai/api/v1/chat/completions \
    --api-key-env OPENROUTER_API_KEY \
    --model anthropic/claude-sonnet-4-20250514
```

Or via config with a preset:

```toml
[agent]
model = "anthropic/claude-sonnet-4-20250514"
provider = "openrouter"

[[providers]]
preset = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-sonnet-4-20250514"
```

### Groq

```sh
export GROQ_API_KEY="gsk_..."
rho --endpoint https://api.groq.com/openai/v1/chat/completions \
    --api-key-env GROQ_API_KEY \
    --model llama-3.3-70b-versatile
```

### DeepInfra

```sh
export DEEPINFRA_API_KEY="di-..."
rho --endpoint https://api.deepinfra.com/v1/openai/chat/completions \
    --api-key-env DEEPINFRA_API_KEY \
    --model meta-llama/Llama-3.3-70B-Instruct
```

### Z.ai

Z.ai provides GLM models (e.g. GLM-5.2) via an OpenAI-compatible API. The endpoint is at `api.z.ai/api/paas/v4/chat/completions` — note the `api.` subdomain and `/api/paas/v4/` path, which differs from the marketing site at `z.ai`.

The `zai` preset also sets a `models_endpoint` at `https://api.z.ai/api/v1/models` (a different path prefix than the chat completions endpoint). This is used automatically — no manual configuration needed.

```sh
export ZAI_API_KEY="..."
rho --endpoint https://api.z.ai/api/paas/v4/chat/completions \
    --api-key-env ZAI_API_KEY \
    --model glm-5.2
```

Or via config with a preset:

```toml
[agent]
model = "glm-5.2"
provider = "zai"

[[providers]]
preset = "zai"
api_key_env = "ZAI_API_KEY"
```

#### Z.ai Coding Plan (subscription)

If you have a **z.ai Coding Plan** subscription (the one built for Claude Code / Cline / similar), use the `zai-coding` preset instead. It targets the subscription endpoint (`/api/coding/paas/v4/...`) and derives the models URL from it (`/api/coding/paas/v4/models`):

```toml
[agent]
model = "glm-5-turbo"
provider = "zai"

[[providers]]
preset = "zai-coding"
api_key_env = "ZAI_API_KEY"
```

> **Don't mix** `preset = "zai"` with a Coding Plan `endpoint` override. If you do, rho now detects the mismatch and derives the models URL from your chat endpoint instead of the platform `/api/v1/models` — so it still works — but `zai-coding` is the cleaner choice.

### Ollama (remote)

If you run Ollama on a different machine, point the endpoint at it:

```sh
rho --endpoint http://192.168.1.100:11434/v1/chat/completions --model llama3
```

## API key handling

API keys are **never** stored in config files. Instead, config references an environment variable name:

```toml
[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
```

At runtime, rho reads the environment variable and sends the key as an `Authorization: Bearer <key>` header with every request. If the variable is not set, the key is silently omitted (local endpoints don't need one).

You can set the key in your shell, in `.bashrc`/`.zshrc`, or in a `.env` file:

```sh
# One-time (current shell)
export OPENAI_API_KEY="sk-..."

# Persistent (add to shell profile)
echo 'export OPENAI_API_KEY="sk-..."' >> ~/.bashrc

# Via .env file (load with direnv or source manually)
echo 'export OPENAI_API_KEY="sk-..."' >> .env
source .env
```

## Token budget tuning

External models often have much larger context windows than local 8K models:

| Provider | Model | Context window |
|---|---|---|
| OpenAI | GPT-4o | 128K tokens |
| Anthropic (via proxy) | Claude Sonnet | 200K tokens |
| Groq | Llama 3.3 70B | 128K tokens |
| DeepInfra | Llama 3.3 70B | 128K tokens |
| Z.ai | GLM-5.2 | 1M tokens |
| Local | Qwen3 8B | Varies (often 32K) |

rho defaults to `token_budget = 32768`. For models with larger windows, increase it:

```toml
[agent]
token_budget = 131072
```

Or via CLI: `--token-budget 131072`.

The budget is split into a **prompt budget** (conversation + system prompt + tool schemas) and a **completion reserve** (room for the model's reply). The default reserve is 8192 tokens.

Run rho with `RUST_LOG=info` to see budget diagnostics at startup:

```
budget: 131072T context, 8192T reserve, 122880T prompt (4700T system + 2000T schema = 6700T overhead, 116180T for conversation)
```

## The `type`, `name`, `preset`, and `api` fields

The `type` field in a provider config is **informational only** — it has no effect on behavior. rho uses `endpoint`, `api`, and `api_key_env` to determine how to connect; it doesn't branch on `type`.

The `name` field is used as the display name in the consent prompt, `/models` output, and provider switching. If not set, rho uses the `type` field, then the endpoint hostname, then the provider index.

The `preset` field auto-fills `endpoint`, `name`, and `api` from a built-in registry. The `openai` preset selects Responses; all other presets select Chat Completions. An explicit `api` value overrides the preset. See the [Configuration](./configuration.md#provider-presets) page for the full list.

```toml
[[providers]]
# Explicit name (optional, set automatically by preset)
name = "my-openrouter"

# Preset fills in endpoint and display name
preset = "openrouter"

# Explicit endpoint overrides preset (optional)
endpoint = "https://..."

# Protocol: "chat_completions" (default) or "responses"
api = "chat_completions"

# API key env var (required for remote providers)
api_key_env = "OPENROUTER_API_KEY"

# Default model for this provider (used when agent.provider points here)
default_model = "anthropic/claude-sonnet-4-20250514"

# Informational type label (has no effect on behavior)
type = "openrouter"
```

If `type` is set to a known unsupported provider name, rho prints a warning at startup. This is a safety net—the real requirement is that the endpoint implements the selected Responses or Chat Completions wire format.

## Connection timeouts

The HTTP client uses sensible defaults to prevent indefinite hangs when a provider is unreachable:

- **Connect timeout**: 30 seconds — how long to wait for the initial TCP/TLS connection
- **Request timeout**: 2 minutes — total time allowed for the entire request (including streaming response)

These are hardcoded and not currently configurable. If you frequently hit these timeouts with a slow local server, ensure the server is running before starting rho.

## Small-model robustness

rho includes safeguards for models that struggle with tool-use tasks:

- **`max_consecutive_empty`** (default 5) — aborts the agent loop when the model returns too many consecutive empty responses, preventing infinite retry spirals. Set to 0 to disable.
- **Sandbox path hints** — when a tool call fails with a sandbox path error, rho appends an actionable hint (sandbox root, correct path format) so the model can self-correct instead of repeating the same mistake.

Both features are configured under `[agent]` — see [Configuration](./configuration.md) for details.

## Privacy considerations

When you use an external provider, **your entire conversation** — including file contents read by the agent, shell command output, and your instructions — is sent to the provider's servers. Review the provider's privacy policy before use.

The consent prompt reminds you of this on every startup for config-driven external endpoints.