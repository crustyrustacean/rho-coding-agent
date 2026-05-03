# Task 10: Provider Switch Warning + Egress Enforcement — Completion Report

**Date:** 2026-05-01 (retroactive)
**Branch:** Commit `815455c`
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Implemented the provider switch warning (user consent for external providers) and egress enforcement (network-level host allowlist) as two complementary security layers.

### 1. Egress enforcement in `LocalChatClient`

**`rho-core/src/client.rs`:**
- Added `egress: Option<EgressConfig>` field to `LocalChatClient`
- `LocalChatClient::with_endpoint_and_egress(endpoint, egress)` — new constructor that enables egress checking
- `check_egress()` — private method that extracts the host from the endpoint URL and verifies it against the allowlist; runs before every `chat()` call
- `LocalChatClient::new()` and `with_endpoint()` retain `egress: None` for backward compatibility (no enforcement)
- When egress is `None`, all hosts are permitted (legacy mode for tests)
- When egress is `Some`, requests to non-allowed hosts are rejected with an error — no network traffic is sent

**`rho-core/src/config.rs`:**
- `EgressConfig::is_host_allowed(host)` — checks if a hostname is permitted
- `localhost`, `127.0.0.1`, and `::1` are always allowed
- Other hosts must appear in `allowed_hosts`
- `RhoConfig::is_host_allowed()` delegates to `EgressConfig::is_host_allowed()`

### 2. Provider switch warning in binary

**`rho/src/main.rs`:**
- `check_provider_consent(endpoint, cli)` — displays a warning and reads `y/N` confirmation when the endpoint is not local
- `is_local_endpoint(endpoint)` — simple string matching for `localhost`, `127.0.0.1`, `[::1]`
- `--accept-external-provider` CLI flag — skips the consent prompt for automated workflows
- Warning message clearly states: "Your prompts and code will be sent to an external server. This may expose proprietary code, secrets, or other sensitive data."
- User must type `y` or `yes` to proceed; any other input aborts

### 3. Integration with startup flow

- Binary constructs `LocalChatClient::with_endpoint_and_egress(endpoint, rho_config.egress.clone())` — both endpoint and egress policy come from config
- Consent check runs before client construction
- Endpoint defaults to `http://localhost:1234/v1/chat/completions` when not configured

---

## Test Coverage

### New unit tests in `client.rs` (8 tests)

| Test | What it verifies |
|---|---|
| `check_egress_allows_localhost_without_config` | No egress config → all hosts OK |
| `check_egress_allows_localhost_with_empty_egress` | Default egress → localhost OK |
| `check_egress_allows_127_0_0_1` | Default egress → 127.0.0.1 OK |
| `check_egress_blocks_unknown_host_by_default` | Default egress → external host blocked |
| `check_egress_allows_listed_host` | Listed host → allowed |
| `check_egress_blocks_unlisted_host_even_when_others_allowed` | Partial allowlist → unlisted blocked |
| `check_egress_no_config_allows_any_host` | Legacy constructor → any host OK |
| `default_endpoint_derives_models_url` | Models URL derived from default endpoint |
| `custom_endpoint_derives_models_url` | Models URL derived from custom endpoint |
| `trailing_slash_endpoint_still_derives_models_url` | Trailing slash handled |

### New unit tests in `config.rs` (4 tests)

| Test | What it verifies |
|---|---|
| `egress_config_localhost_always_allowed` | localhost/127.0.0.1/::1 always OK |
| `egress_config_unknown_host_denied_by_default` | Unknown hosts blocked by default |
| `egress_config_allowed_hosts_permitted` | Listed hosts allowed |
| `egress_config_multiple_allowed_hosts` | Multiple hosts work |

### New integration tests (6 tests)

| Test | What it verifies |
|---|---|
| `egress_blocks_external_host_with_default_config` | End-to-end: external host blocked by egress |
| `egress_allows_localhost_with_default_config` | End-to-end: localhost gets HTTP error (not egress) |
| `egress_allows_listed_external_host` | End-to-end: listed host bypasses egress |
| `egress_blocks_unlisted_external_host` | End-to-end: unlisted host blocked |
| `egress_allows_127_0_0_1_with_default_config` | End-to-end: 127.0.0.1 gets HTTP error (not egress) |
| `egress_no_config_allows_any_host` | End-to-end: legacy constructor permits any host |

### New unit tests in `main.rs` (6 tests)

| Test | What it verifies |
|---|---|
| `local_endpoint_localhost` | localhost detected as local |
| `local_endpoint_127_0_0_1` | 127.0.0.1 detected as local |
| `local_endpoint_ipv6_loopback` | [::1] detected as local |
| `external_endpoint_openai` | OpenAI detected as external |
| `external_endpoint_anthropic` | Anthropic detected as external |
| `local_endpoint_case_insensitive` | Case-insensitive local detection |

---

## File Changes

| File | Change |
|---|---|
| `rho-core/src/client.rs` | Added `egress` field, `with_endpoint_and_egress()`, `check_egress()`, `list_models()`, `ModelInfo`, `ModelList`, `enhance_http_body()`, `truncate_error_body()`, 8 unit tests |
| `rho-core/src/config.rs` | Added `EgressConfig::is_host_allowed()`, refactored `RhoConfig::is_host_allowed()` to delegate, 4 unit tests |
| `rho-core/src/lib.rs` | Re-export `ModelInfo`, `ModelList` |
| `rho/src/main.rs` | Added `check_provider_consent()`, `is_local_endpoint()`, `--accept-external-provider` flag, `--model` flag, config-driven endpoint selection, 6 unit tests |

---

## Design Decisions

1. **Egress enforcement at the client, not the transport** — Checking happens inside `LocalChatClient::chat()` before the HTTP request is made. This means no network traffic is sent to blocked hosts, and the error is deterministic (no DNS or timeout variability).

2. **Consent is binary-level, not provider-level** — The consent prompt lives in `rho/src/main.rs`, not in the `ChatClient` trait. Consent is a user-facing concern, not a provider capability. A different binary (e.g., a daemon) could make different consent decisions.

3. **`is_local_endpoint()` uses string matching** — Rather than pulling in the `url` crate at the binary level, simple string matching handles the three local host patterns. This is sufficient for the consent use case.

4. **Legacy constructors remain unenforced** — `LocalChatClient::new()` and `with_endpoint()` have `egress: None`, allowing all hosts. This preserves backward compatibility for tests and direct usage. The binary always uses `with_endpoint_and_egress()`.

5. **Egress config is per-client** — The `EgressConfig` is set at construction time and applies to all requests through that client. Changing the egress policy requires constructing a new client.

---

## Out of Scope (deferred)

| Item | Deferred to |
|---|---|
| Per-request host overrides | Future |
| Wildcard or regex-based host matching | Future |
| SOCKS proxy or VPN detection | Future |
