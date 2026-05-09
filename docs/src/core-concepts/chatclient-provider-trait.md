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

The default implementation targets `localhost:1234` (LM Studio / Ollama):

```rust
// Default: localhost with no egress enforcement
let client = LocalChatClient::new();

// Custom endpoint with egress allowlist
let client = LocalChatClient::with_endpoint_and_egress(
    "http://localhost:1234/v1/chat/completions",
    egress_config,
);
```

### Egress enforcement

When constructed with `with_endpoint_and_egress`, the client checks the resolved host of the endpoint URL against the egress allowlist before every request. Requests to non-allowed hosts are refused with `RhoError::EgressBlocked`.

`localhost`, `127.0.0.1`, and `::1` are always allowed without configuration. Any other host must appear in `allowed_hosts` in the egress config.

### Provider consent

The binary (`rho`) checks whether the configured endpoint is local before connecting. Non-local endpoints (e.g., `api.openai.com`) trigger an interactive consent warning. Use `--accept-external-provider` to skip this in automated workflows.

## Model resolution

The model identifier is resolved in priority order:

1. **Config** — `agent.model` in `.rho/config.toml` or `~/.rho/config.toml`
2. **CLI flag** — `--model <name>`
3. **Auto-detect** — query the server's `/v1/models` endpoint, use the first loaded model

Auto-detection fails with a clear error if the server is unreachable or has no models loaded.

## Request / response

| Type | Purpose |
|---|---|
| `ChatRequest` | `model`, `messages`, `tools` — the full API request body |
| `ModelResponse` | Parsed API response with choices, finish reason, usage |
| `FinishReason` | `Stop` (text reply), `ToolCalls`, `Length` (truncated), `ContentFilter` |
| `ModelUsage` | `prompt_tokens`, `completion_tokens`, `total_tokens` |

## Streaming

`ToolOutcome::Streamed` is declared for future streaming support (Phase 4). The current implementation uses synchronous request → full response.
