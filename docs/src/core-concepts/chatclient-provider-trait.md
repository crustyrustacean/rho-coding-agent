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
// Default: localhost with no egress enforcement
let client = LocalChatClient::new();

// Custom endpoint with egress allowlist and API key
let client = LocalChatClient::with_endpoint_egress_and_key(
    "https://api.openai.com/v1/chat/completions",
    egress_config,
    Some("sk-...".into()),
);
```

### Egress enforcement

When constructed with `with_endpoint_and_egress`, the client checks the resolved host of the endpoint URL against the egress allowlist before every request. Requests to non-allowed hosts are refused with `RhoError::EgressBlocked`.

`localhost`, `127.0.0.1`, and `::1` are always allowed without configuration. Any other host must appear in `allowed_hosts` in the egress config.

### Bearer authentication

When an API key is provided via `api_key_env` in config (or `with_endpoint_egress_and_key` in code), it is sent as an `Authorization: Bearer <key>` header with every request. Local endpoints typically don't need this. See [External Providers](./providers.md) for setup details.

### Provider consent

The binary (`rho`) checks whether the configured endpoint is local before connecting. Non-local endpoints (e.g., `api.openai.com`) trigger an interactive consent warning. Use `--accept-external-provider` to skip this in automated workflows.

### Egress + consent: the two-gate requirement

External providers must satisfy **both** gates:

1. **Consent** — `--accept-external-provider` or type `y` at the prompt
2. **Egress allowlist** — the host must appear in `[egress] allowed_hosts`

If you consent but the host isn't allowlisted, the request is blocked with `RhoError::EgressBlocked`. If you allowlist the host but don't consent, the prompt appears and the process exits unless you answer. Both must be configured for external providers to work.

## Model resolution

The model identifier is resolved in priority order:

1. **Config** — `agent.model` in `.rho/config.toml` or `~/.rho/config.toml`
2. **CLI flag** — `--model <name>`
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
