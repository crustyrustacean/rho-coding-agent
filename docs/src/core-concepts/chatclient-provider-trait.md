# Provider Architecture

rho communicates with LLMs through a layered provider abstraction in `rho-ai`.

## `LlmService` trait

The core trait in `rho-ai` defines provider-agnostic LLM communication:

```rust
#[async_trait]
pub trait LlmService: Send + Sync {
    fn chat_stream(
        &self,
        request: LlmRequest,
    ) -> Result<EventStream, ProviderError>;
}
```

All providers implement this trait. The agent loop consumes the `EventStream` (a `Pin<Box<dyn Stream<Item = Result<StreamEvent>>>>`) for streaming responses.

## `OpenAiService`

The default implementation targets any OpenAI-compatible endpoint:

```rust
use rho_ai::OpenAiService;

// Default: localhost
let service = OpenAiService::new(rho_ai::ProviderConfig::new(
    "",  // no API key for local
    "http://localhost:1234/v1",\n));

// Custom endpoint with API key
let service = OpenAiService::new(rho_ai::ProviderConfig::new(
    "sk-...",
    "https://api.openai.com/v1",\n));
```

The model is specified per-request via `LlmRequest::model`, not in the provider config. This ensures the model always comes from the session, never from a stale config value.

### [REDACTED]

When an API key is provided via `api_key_env` in config, it is sent as an `Authorization: Bearer <key>` header with every request. Local endpoints typically don't need this. See [External Providers](../providers.md) for setup details.

### Provider consent

The binary (`rho`) checks whether the configured endpoint is local before connecting. Non-local endpoints (e.g., `api.openai.com`) trigger an interactive consent warning that lists the external provider(s) by name. If no local server is detected, the warning also notes this. Use `--accept-external-provider` to skip this in automated workflows, or `--endpoint` (which implies consent since the user explicitly chose the target).

### Model picker

When no models can be auto-detected (all providers unreachable or `/v1/models` unsupported), rho offers an interactive model picker with curated models from Anthropic, OpenAI, and z.ai. The user can also enter a model ID manually. This fallback only fires when neither `--model` nor `agent.model` is set and auto-detection fails.

## Model resolution

The model identifier is resolved in priority order:

1. **CLI flag** — `--model <name>` (highest priority)
2. **Config** — `agent.model` in `.rho/config.toml` or `~/.rho/config.toml`
3. **Auto-detect** — query the server's `/v1/models` endpoint, use the first loaded model

Auto-detection works well for local servers where `/v1/models` is reliable. **For external providers, always specify the model explicitly** with `--model` or in config — external providers may return large model lists or non-standard responses that can cause auto-detection to pick an unexpected model or fail.

## Request / response

| Type | Purpose |
|---|---|
| `LlmRequest` | `model`, `messages`, `tools` — the full API request body |
| `StreamEvent` | Streaming response event (`Text`, `Reasoning`, `ToolUseStart/Delta/Complete`, `Done`) |
| `AccumulatedResponse` | Fully-accumulated response (text + tool calls + usage) |
| `FinishReason` | `Stop` (text reply), `ToolCalls`, `Length` (truncated), `ContentFilter` |
| `LlmUsage` | `prompt_tokens`, `completion_tokens`, `total_tokens` |

## Streaming

`OpenAiService` uses SSE streaming (`stream: true`) for the Chat Completions API. Streaming events are returned as an `EventStream` that the agent loop consumes. `StreamEvent::Text` and `StreamEvent::Reasoning` events are forwarded to the `AgentObserver` in real time, enabling live progress output in the REPL.

## Multi-provider support

rho can manage multiple providers simultaneously via the `ProviderRegistry` and `Provider` trait:

- Each provider has a name, endpoint, and optional API key (configured via `[[providers]]` with optional `preset`).
- The registry scans all providers to find which one serves a given model.
- `/models` lists all models across all configured providers.
- `/model <id>` switches to a model, **automatically selecting the right provider** based on which provider serves that model.
- Project-level providers merge with user-level providers by name, so you can configure providers once globally and override selectively per project.

Example with multiple providers:

```toml
# ~/.rho/config.toml
[[providers]]
preset = "lm-studio"

[[providers]]
preset = "openrouter"
api_key_env = "OPENROUTER_API_KEY"

[agent]
model = "deepseek-v4-flash"  # served by openrouter
```

See [Configuration](../configuration.md#provider-presets) for preset details and [External Providers](../providers.md) for provider setup.
