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
    "",                          // no API key for local
    "http://localhost:1234/v1",  // base URL
));

// Custom endpoint with API key
let service = OpenAiService::new(rho_ai::ProviderConfig::new(
    "sk-...",
    "https://api.openai.com/v1",
));
```

The model is specified per-request via `LlmRequest::model`, not in the provider config. This ensures the model always comes from the session, never from a stale config value.

### Authentication

When an API key is provided via `api_key_env` in config, it is sent as an `Authorization: Bearer <key>` header with every request. Local endpoints typically don't need this. See [External Providers](../providers.md) for setup details.

### Provider consent

The binary (`rho`) checks whether the configured endpoint is local before connecting. Non-local endpoints (e.g., `api.openai.com`) trigger a consent warning that lists the external provider(s) by name and exits with an error unless the user passes `--accept-external-provider` or `--endpoint` (which implies consent since the user explicitly chose the target). This is a headless consent gate — there is no interactive prompt.

### Model picker

When no model can be resolved from config or CLI, rho falls back to a built-in default model (`anthropic/claude-sonnet-4` via OpenRouter). Set `--model <id>`, `agent.model`, or `agent.provider` with a `default_model` on the provider to use a specific model. The built-in default requires an OpenRouter API key or a configured provider.

## Model resolution

The model identifier is resolved in priority order:

1. **CLI flag** — `--model <name>` (highest priority)
2. **Config model** — `agent.model` in `.rho/config.toml` or `~/.rho/config.toml`
3. **Config provider** — `agent.provider` selects a provider and uses its `default_model`
4. **Provider default** — the first provider's `default_model` if configured

rho does **not** call `/v1/models` at startup. The config file is the source of truth — model names are passed directly to the provider, and misconfiguration surfaces as a clear HTTP error at request time.

## Request / response

| Type | Purpose |
|---|---|
| `LlmRequest` | `model`, `messages`, `tools` — the full API request body |
| `StreamEvent` | Streaming response event (`Text`, `Reasoning`, `ToolUseStart/Delta/Complete`, `Done`) |
| `AccumulatedResponse` | Fully-accumulated response (text + tool calls + usage) |
| `FinishReason` | `Stop` (text reply), `ToolCalls`, `Length` (truncated), `ContentFilter`, `Other` |
| `LlmUsage` | `prompt_tokens`, `completion_tokens`, `total_tokens` |

## Streaming

`OpenAiService` uses SSE streaming (`stream: true`) for the Chat Completions API. Streaming events are returned as an `EventStream` that the agent loop consumes. `StreamEvent::Text` and `StreamEvent::Reasoning` events are forwarded to the `AgentObserver` in real time, enabling live progress output in the consumer.

## Multi-provider support

rho can manage multiple providers simultaneously via the `ProviderRegistry` and `Provider` trait. Each provider encapsulates identity, externality, model discovery, and access to an `LlmService`.

- Each provider has a name, endpoint, and optional API key (configured via `[[providers]]` with optional `preset`).
- Providers can set `models_endpoint` when the models listing is at a different path prefix than chat completions (e.g. Z.ai serves models at `/api/v1/models` but chat at `/api/paas/v4/`). When unset, the models URL is derived from the chat endpoint.
- Model list responses are parsed flexibly via `serde untagged` enums: `ModelList` accepts both `{"data": [...]}` (standard OpenAI) and `{"models": [...]}` (Z.ai). `ModelInfo` accepts both `id` and `slug` fields.
- The registry scans all providers to find which one serves a given model.
- `listModels` lists all models across all configured providers.
- `setModel` switches to a model, **automatically selecting the right provider** based on which provider serves that model.
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