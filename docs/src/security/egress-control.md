# Egress Control

Egress control restricts which hosts the agent can contact over the network. It prevents the model from exfiltrating data to arbitrary internet endpoints.

## Allowlist

The egress allowlist is configured in `EgressConfig`:

```toml
[egress]
allowed_hosts = ["api.openai.com"]
```

Three hosts are always allowed without configuration:

| Host | Reason |
|---|---|
| `localhost` | Local model servers (LM Studio, Ollama) |
| `127.0.0.1` | Loopback IPv4 |
| `::1` | Loopback IPv6 |

Any other host must appear in `allowed_hosts`. Requests to non-allowed hosts are refused with `RhoError::EgressBlocked`.

## Where it's enforced

### `LocalChatClient`

The model API client checks the egress allowlist before every `chat()` and `list_models()` call. This prevents the model provider from being switched to an unauthorized endpoint.

### Provider consent (binary level)

The binary (`rho`) checks whether the configured endpoint is local before connecting. Non-local endpoints trigger an interactive consent warning:

```text
  ⚠  External provider detected
      Endpoint: https://api.openai.com/v1/chat/completions

      Your prompts and code will be sent to an external server.
      Continue? [y/N]
```

Use `--accept-external-provider` to skip this in automated workflows.

### Shell denylist (complementary)

The command denylist in `RunCommand` blocks common exfiltration tools (`curl`, `wget`, `Invoke-WebRequest`, `Invoke-RestMethod`, etc.) regardless of the egress config. This is a separate defense layer — the denylist catches shell-level attempts, the egress allowlist catches API-level attempts.

## Adding new outbound tools

When adding a tool that makes HTTP requests (e.g., a crates.io lookup), the tool must check the egress allowlist before every request. The `EgressConfig::is_host_allowed()` method provides this check:

```rust
if !config.is_host_allowed("crates.io") {
    return Err(RhoError::EgressBlocked { host: "crates.io".into() });
}
```

The user must add the target host to their `allowed_hosts` config before the tool will work.
