# ChatClient Provider Trait

The `ChatClient` trait abstracts the model API. It defines how rho talks to LLMs — any OpenAI-compatible endpoint can be plugged in.

## Definition

```rust
#[async_trait]
pub trait ChatClient: Send + Sync {
    /// Send a chat completion request and return the model's response.
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse>;

    /// List available models from the /v1/models endpoint.
    async fn list_models(&self) -> Result<ModelList>;
}
```

## `LocalChatClient`

The default implementation targets `localhost:1234` (LM Studio / Ollama) by default, but works with any OpenAI-compatible endpoint:

```rust
// Default: localhost
let client = LocalChatClient::new();

// Custom endpoint with optional API key
let client = LocalChatClient::with_endpoint_and_key(
    "https://api.openai.com/v1/chat/completions",
    Some("sk-...".into()),
);
```

### Bearer authentication

When an API key is provided via `api_key_env` in config (or `with_endpoint_and_key` in code), it is sent as an `Authorization: Bearer <key>` header with every request. Local endpoints typically don't need this. See [External Providers](../providers.md) for setup details.

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
| `ChatRequest` | `model`, `messages`, `tools` — the full API request body |
| `ModelResponse` | Parsed API response with choices, finish reason, usage |
| `FinishReason` | `Stop` (text reply), `ToolCalls`, `Length` (truncated), `ContentFilter` |
| `ModelUsage` | `prompt_tokens`, `completion_tokens`, `total_tokens` |

## Streaming

`ToolOutcome::Streamed` is declared for future streaming support (Phase 4). The current implementation uses synchronous request → full response.
