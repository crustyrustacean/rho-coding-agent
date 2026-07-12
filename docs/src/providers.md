# External Providers

rho's `OpenAiService` (in `rho-ai`) speaks the **OpenAI Chat Completions wire format** (`POST /v1/chat/completions`). It works with any endpoint that implements this format — including local servers (LM Studio, Ollama) and external providers that offer an OpenAI-compatible API.

## What "OpenAI-compatible" means

The request body rho sends follows the OpenAI specification:

```json
{
  "model": "gpt-4o",
  "messages": [...],
  "tools": [...],
  "temperature": ...
}
```

And rho expects the response to follow the same specification (choices with `finish_reason`, `message.content`, `message.tool_calls`, usage stats). **Any endpoint that accepts and returns this format works.**

### Providers that work natively
These providers expose an OpenAI-compatible API as their primary interface:

| Provider | Endpoint | Notes |
|---|---|---|
| OpenAI | `api.openai.com/v1/chat/completions` | Native OpenAI format |
| OpenRouter | `openrouter.ai/api/v1/chat/completions` | Proxies 100+ models under one endpoint |
| Groq | `api.groq.com/openai/v1/chat/completions` | OpenAI-compatible path |
| DeepInfra | `api.deepinfra.com/v1/openai/chat/completions` | OpenAI-compatible path |
| LM Studio | `localhost:1234/v1/chat/completions` | Local server |
| Ollama | `localhost:11434/v1/chat/completions` | Local server |
| Together AI | `api.together.xyz/v1/chat/completions` | OpenAI-compatible path |
| Fireworks | `api.fireworks.ai/inference/v1/chat/completions` | OpenAI-compatible path |
| Z.ai | `api.z.ai/api/paas/v1/chat/completions` | OpenAI-compatible path (GLM models) |

### Providers that do **not** work directly

These providers use their own API format and **cannot be used with rho** without an OpenAI-compatible proxy:

| Provider | Native endpoint | Why it doesn't work |
|---|---|---|
| Anthropic | `/v1/messages` | Uses the Messages API, not Chat Completions |
| Google Gemini | `/v1beta/models/...:generateContent` | Uses GenerateContent, not Chat Completions |
| AWS Bedrock | `/model/.../converse` | Uses the Converse API |
| Cohere | `/v2/chat` | Different request/response format |

**To use models from these providers**, you need an [OpenAI-compatible proxy](https://github.com/BerriAI/litellm) that translates between their native API and the OpenAI wire format. For example, LiteLLM or OpenRouter can proxy Claude models behind an OpenAI-compatible endpoint.

rho detects common misconfigurations at startup and prints a warning if the endpoint path doesn't look OpenAI-compatible or if `provider.type` is set to a non-compatible provider name.

## Prerequisites

To use an external provider you need two things:

1. **An API key** from the provider
2. **The provider's endpoint URL** (must be OpenAI-compatible)

That's it. Configure via config file or CLI flags — see below.

## Quick start: OpenAI

### Via CLI flags

```sh
export OPENAI_API_KEY="sk-..."

rho --endpoint https://api.openai.com/v1/chat/completions \
    --api-key-env OPENAI_API_KEY \
    --model gpt-4o
```

`--endpoint` implies consent (the consent prompt is skipped since you explicitly chose the target).

### Via config

```toml
# ~/.rho/config.toml
[agent]
model = "gpt-4o"
provider = "openai"
token_budget = 131072

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
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

```sh
export OPENAI_API_KEY="sk-..."
rho --endpoint https://api.openai.com/v1/chat/completions \
    --api-key-env OPENAI_API_KEY \
    --model gpt-4o
```

Or via config with a preset:

```toml
[agent]
model = "gpt-4o"
provider = "openai"

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
```

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
rho --endpoint https://api.z.ai/api/paas/v1/chat/completions \
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

The budget is split into a **prompt budget** (conversation + system prompt + tool schemas) and a **completion reserve** (room for the model's reply). The default reserve is 4096 tokens.

Run rho with `RUST_LOG=info` to see budget diagnostics at startup:

```
budget: 131072T context, 4096T reserve, 126976T prompt (4700T system + 2000T schema = 6700T overhead, 120276T for conversation)
```

## The `type`, `name`, and `preset` fields

The `type` field in a provider config is **informational only** — it has no effect on behavior. rho uses `endpoint` and `api_key_env` to determine how to connect; it doesn't branch on `type`.

The `name` field is used as the display name in the consent prompt, `/models` output, and provider switching. If not set, rho uses the `type` field, then the endpoint hostname, then the provider index.

The `preset` field auto-fills `endpoint` and `name` from a built-in registry. See the [Configuration](./configuration.md#provider-presets) page for the full list of presets.

```toml
[[providers]]
# Explicit name (optional, set automatically by preset)
name = "my-openrouter"

# Preset fills in endpoint and display name
preset = "openrouter"

# Explicit endpoint overrides preset (optional)
endpoint = "https://..."

# API key env var (required for remote providers)
api_key_env = "OPENROUTER_API_KEY"

# Default model for this provider (used when agent.provider points here)
default_model = "anthropic/claude-sonnet-4-20250514"

# Informational type label (has no effect on behavior)
type = "openrouter"
```

If `type` is set to a known non-OpenAI-compatible provider name (e.g. `"anthropic"`, `"google"`, `"bedrock"`), rho will print a warning at startup. This is a safety net — the real check is that your endpoint accepts and returns the OpenAI Chat Completions format.

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