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
token_budget = 131072

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
```

```sh
export OPENAI_API_KEY="sk-..."
rho --accept-external-provider
```

## Provider consent

When connecting to a non-local endpoint, rho displays an interactive consent warning before any data leaves your machine:

```text
  ⚠  No local model server detected
  ⚠  External provider(s) configured:
      - openrouter

      Your prompts and code will be sent to external servers.
      This may expose proprietary code, secrets, or other
      sensitive data to the providers and any intermediaries.

      Continue? [y/N]
```

If rho also has a local provider (e.g. LM Studio is running alongside an external provider), the "No local model server detected" header is omitted.

The consent prompt fires automatically for config-driven external endpoints. It is **skipped** when you use `--endpoint` on the CLI (explicit endpoint implies consent) or `--accept-external-provider`.

## Specifying the model

There are three ways to set the model, in priority order:

1. **CLI flag** — `--model gpt-4o` (highest priority)
2. **Config** — `[agent] model = "gpt-4o"`
3. **Auto-detection** — query the server's `/v1/models` endpoint and use the first loaded model

Auto-detection works for local servers (LM Studio, Ollama) where `/v1/models` is reliable. For external providers, **always specify the model explicitly** with `--model` or in config.

### Interactive model picker

When rho has an external provider configured but cannot list models (e.g. `/v1/models` times out, the API key is wrong, or the provider doesn't support model listing), rho offers an interactive model picker instead of aborting:

```text
  Could not list models from: openrouter
  Select a model to use:

    [1] Claude Sonnet 4        (Anthropic)
    [2] GPT-4o                 (OpenAI)
    [3] GLM-5                  (z.ai)
    [0] Enter model ID manually

  Choice:
```

The picker offers one recommended model from each of Anthropic, OpenAI, and z.ai. Selecting `[0]` lets you type any model ID. You can also type a model ID directly instead of a number.

The curated models use `OpenRouter` model IDs (e.g. `anthropic/claude-sonnet-4`) which work with any `OpenRouter`-compatible endpoint. For direct OpenAI or Anthropic API access, use `[0]` to enter the native model ID (e.g. `gpt-4o` or `claude-sonnet-4-20250514`).

To skip the picker entirely, specify `--model` on the CLI or set `agent.model` in config.

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

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
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

[[providers]]
preset = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
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

# Informational type label (has no effect on behavior)
type = "openrouter"
```

If `type` is set to a known non-OpenAI-compatible provider name (e.g. `"anthropic"`, `"google"`, `"bedrock"`), rho will print a warning at startup. This is a safety net — the real check is that your endpoint accepts and returns the OpenAI Chat Completions format.

## Privacy considerations

When you use an external provider, **your entire conversation** — including file contents read by the agent, shell command output, and your instructions — is sent to the provider's servers. Review the provider's privacy policy before use.

The consent prompt reminds you of this on every startup for config-driven external endpoints.
