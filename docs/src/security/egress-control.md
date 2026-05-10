# Egress Control

> **Removed in v0.33.3.** Egress enforcement was removed because it only gated one HTTP client (`LocalChatClient`) while shell commands had unrestricted network access via `curl`, `Invoke-WebRequest`, etc. The provider consent warning is the real defense for external providers.

## What was removed

- `EgressConfig` struct and `[egress]` config section
- `RhoError::EgressBlocked` error variant
- `check_egress()` method on `LocalChatClient`
- `with_endpoint_and_egress()` and `with_endpoint_egress_and_key()` constructors
- Egress allowlist checks before every `chat()` and `list_models()` call

## Why it was removed

The egress allowlist created a false sense of security:

1. **Shell bypass:** The model (or a prompt injection attack) could exfiltrate data via shell commands (`curl`, `Invoke-WebRequest`, etc.) regardless of the egress config.
2. **Limited scope:** Egress only gated `LocalChatClient` HTTP requests. It did not cover any other network access path.
3. **The consent warning already covers this:** When connecting to a non-local endpoint, rho displays an interactive consent prompt warning that data will leave the machine. This is the meaningful defense — the user explicitly approves where their data goes.

## What remains

The **command denylist** in `RunCommand` blocks common exfiltration tools (`curl`, `wget`, `Invoke-WebRequest`, `Invoke-RestMethod`, etc.). This is still active and provides practical protection against the most common exfiltration vectors.

The **provider consent warning** fires before any connection to a non-local endpoint. Use `--accept-external-provider` to skip it in automated workflows, or `--endpoint` (which implies consent since the user explicitly chose the target).
