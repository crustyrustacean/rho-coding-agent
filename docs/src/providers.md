# External Providers

rho's `LocalChatClient` speaks the **OpenAI Chat Completions wire format** (`POST /v1/chat/completions`). It works with any endpoint that implements this format — including local servers (LM Studio, Ollama) and external providers that offer an OpenAI-compatible API.

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
| Mistral (direct) | `/v1/chat` | Different response schema |
| Cohere | `/v2/chat` | Different request/response format |

**To use models from these providers**, you need an [OpenAI-compatible proxy](https://github.com/BerriAI/litellm) that translates between their native API and the OpenAI wire format. For example, LiteLLM or OpenRouter can proxy Claude models behind an OpenAI-compatible endpoint.

rho detects common misconfigurations at startup and prints a warning if the endpoint path doesn't look OpenAI-compatible or if `provider.type` is set to a non-compatible provider name.

## Prerequisites

To use an external provider you need three things:

1. **An API key** from the provider
2. **The provider's endpoint URL** (must be OpenAI-compatible)
3. **An egress allowlist entry** so rho's network guard permits the connection

## Quick start: OpenAI

```sh
# Set your API key as an environment variable
export OPENAI_API_KEY="sk-..."

# Run rho against OpenAI (consent prompt appears on first use)
rho --model gpt-4o --accept-external-provider
```

The corresponding config (`.rho/config.toml` or `~/.rho/config.toml`):

```toml
[agent]
model = "gpt-4o"
token_budget = 131072

[provider]
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"

[egress]
allowed_hosts = ["api.openai.com"]
```

## The two-gate requirement

External providers must pass **both** gates before any request is sent:

| Gate | What it does | How to satisfy |
|---|---|---|
| **Egress allowlist** | Blocks requests to hosts not in `allowed_hosts` | Add the hostname to `[egress] allowed_hosts` |
| **Provider consent** | Interactive warning that data will leave your machine | Type `y` at the prompt, or use `--accept-external-provider` |

If you consent but the host isn't in the allowlist, the request is silently blocked with `RhoError::EgressBlocked`. **Both must be configured.** If you see egress errors after consenting, check that the hostname matches exactly.

`localhost`, `127.0.0.1`, and `::1` are always allowed and never trigger consent.

## Provider examples

### OpenAI

```toml
[agent]
model = "gpt-4o"

[provider]
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"

[egress]
allowed_hosts = ["api.openai.com"]
```

```sh
export OPENAI_API_KEY="sk-..."
rho --model gpt-4o --accept-external-provider
```

### OpenRouter

OpenRouter proxies many models behind a single endpoint. Model names include the provider prefix (e.g. `anthropic/claude-sonnet-4-20250514`).

```toml
[agent]
model = "anthropic/claude-sonnet-4-20250514"

[provider]
endpoint = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"

[egress]
allowed_hosts = ["openrouter.ai"]
```

```sh
export OPENROUTER_API_KEY="sk-or-..."
rho --model anthropic/claude-sonnet-4-20250514 --accept-external-provider
```

### Groq

```toml
[agent]
model = "llama-3.3-70b-versatile"

[provider]
endpoint = "https://api.groq.com/openai/v1/chat/completions"
api_key_env = "GROQ_API_KEY"

[egress]
allowed_hosts = ["api.groq.com"]
```

```sh
export GROQ_API_KEY="gsk_..."
rho --model llama-3.3-70b-versatile --accept-external-provider
```

### DeepInfra

```toml
[agent]
model = "meta-llama/Llama-3.3-70B-Instruct"

[provider]
endpoint = "https://api.deepinfra.com/v1/openai/chat/completions"
api_key_env = "DEEPINFRA_API_KEY"

[egress]
allowed_hosts = ["api.deepinfra.com"]
```

```sh
export DEEPINFRA_API_KEY="di-..."
rho --model meta-llama/Llama-3.3-70B-Instruct --accept-external-provider
```

### Ollama (remote)

If you run Ollama on a different machine, point the endpoint at it:

```toml
[provider]
endpoint = "http://192.168.1.100:11434/v1/chat/completions"

[egress]
allowed_hosts = ["192.168.1.100"]
```

No `api_key_env` is needed for Ollama. No consent prompt fires because the host isn't `localhost` but is in the allowlist — however, rho's consent check only fires for *non-local* endpoints (anything not `localhost`/`127.0.0.1`/`::1`), so a LAN IP will trigger the consent warning. Use `--accept-external-provider` to skip it.

## API key handling

API keys are **never** stored in config files. Instead, config references an environment variable name:

```toml
[provider]
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

See [What "OpenAI-compatible" means](#what-openai-compatible-means) above — models from Anthropic, Google, AWS Bedrock, and others can only be used through an OpenAI-compatible proxy.

rho defaults to `token_budget = 32768`. For models with larger windows, increase it to take advantage of the extra space:

```toml
[agent]
token_budget = 131072
```

The budget is split into a **prompt budget** (conversation + system prompt + tool schemas) and a **completion reserve** (room for the model's reply). The default reserve is 4096 tokens. Set the budget high enough that the system prompt (~4,700 tokens) plus tool schemas (~2,000 tokens) plus a few tool-call rounds still fit comfortably.

Run rho with `RUST_LOG=info` to see budget diagnostics at startup:

```
budget: 131072T context, 4096T reserve, 126976T prompt (4700T system + 2000T schema = 6700T overhead, 120276T for conversation)
```

## The `provider.type` field

The `type` field in `[provider]` is **informational only** — it has no effect on behavior. rho uses `endpoint` and `api_key_env` to determine how to connect; it doesn't branch on `type`.

```toml
[provider]
endpoint = "https://..." # this is what actually matters
api_key_env = "OPENAI_API_KEY"
# type is optional — set it for your own bookkeeping or omit it
```

You can set `type` to any string for your own bookkeeping (e.g. `"openrouter"`, `"groq"`, `"production"`), or omit it entirely. Rho does not validate it against the endpoint.

**However**, if you set `type` to a known non-OpenAI-compatible provider name (e.g. `"anthropic"`, `"google"`, `"bedrock"`), rho will print a warning at startup reminding you that only OpenAI-compatible endpoints are supported. This is a safety net — the real check is that your endpoint accepts and returns the OpenAI Chat Completions format.

## Specifying the model

There are three ways to set the model, in priority order:

1. **CLI flag** — `--model gpt-4o` (highest priority)
2. **Config** — `[agent] model = "gpt-4o"`
3. **Auto-detection** — query the server's `/v1/models` endpoint and use the first loaded model

Auto-detection works for local servers (LM Studio, Ollama) where `/v1/models` is reliable. For external providers, **always specify the model explicitly** with `--model` or in config. External providers may return large model lists or use non-standard `/v1/models` responses, which can cause auto-detection to pick an unexpected model or fail.

## Privacy considerations

When you use an external provider, **your entire conversation** — including file contents read by the agent, shell command output, and your instructions — is sent to the provider's servers. Review the provider's privacy policy before use.

The consent prompt reminds you of this on every startup (unless you use `--accept-external-provider`). The egress allowlist ensures that only hosts you've explicitly approved are contacted.
