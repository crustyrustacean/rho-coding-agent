# Phase 3.11 Tasks

**Status:** ✅ Complete

---

## 1. Create `rho-core/src/provider.rs` with `Provider` trait ✅

**File:** `rho-core/src/provider.rs` (new)

Defined the trait with 5 methods:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn is_external(&self) -> bool;
    async fn list_models(&self) -> Result<ModelList>;
    fn chat_client(&self) -> &dyn ChatClient;
    fn clone_boxed_client(&self) -> Box<dyn ChatClient>;
}
```

- `name()` — human-readable provider name for consent prompts and model lists
- `is_external()` — derived from endpoint URL at construction time
- `list_models()` — returns full `ModelList` (top-level fields preserved for future use)
- `chat_client()` — borrowed reference to the underlying `ChatClient`
- `clone_boxed_client()` — owned client for bench wrapping and other ownership-transfers

## 2. Implement `OpenAiCompatibleProvider` ✅

**File:** `rho-core/src/provider.rs`

```rust
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleProvider {
    client: LocalChatClient,
    is_external: bool,
}
```

- `new(endpoint, api_key)` — constructs from endpoint + optional bearer auth
- `is_external` derived from endpoint URL via `is_local_endpoint()`
- `list_models()` delegates to `client.list_models()` directly (returns `ModelList`)
- `clone_boxed_client()` uses `LocalChatClient::clone()` → `Box::new()`

## 3. Implement `provider_factory()` ✅

**File:** `rho-core/src/provider.rs`

```rust
pub fn provider_factory(
    config: &RhoConfig,
    endpoint_override: Option<&str>,
    api_key_env_override: Option<&str>,
) -> Box<dyn Provider>
```

Mirrors `client_factory()` but returns `Box<dyn Provider>`:
- Endpoint priority: CLI override → config `provider.endpoint` → `DEFAULT_ENDPOINT` (localhost:1234)
- API key priority: CLI override env var name → config `provider.api_key_env`

## 4. Wire module and re-exports in `rho-core/src/lib.rs` ✅

Added:
```rust
pub mod provider;
pub use provider::{OpenAiCompatibleProvider, Provider, provider_factory};
```

## 5. Refactor `rho/src/app.rs` — `App` holds `Box<dyn Provider>` ✅

Changes:
- `App.client: LocalChatClient` → `App.provider: Box<dyn Provider>`
- Merged old steps 4+5+9 into single step 4 (`provider_factory`)
- Deleted `resolve_endpoint()` (was ~15 lines, now handled by `provider_factory`)
- Replaced `check_provider_compatibility()` with simpler `check_provider_type()` (type-label warning only, dropped path-endpoint check)
- `check_provider_consent()` signature: `provider: &dyn Provider` — uses `provider.is_external()` and `provider.name()`
- `resolve_model()` signature: `provider: &dyn Provider` — uses `provider.list_models()`
- Consent prompt shows "Provider: OpenAI Compatible" instead of raw endpoint URL

## 6. Update `rho/src/repl.rs` call sites ✅

Two changes:
- `run_repl`: `&app.client` → `app.provider.chat_client()`
- `run_prompt_file`: `&app.client` → `app.provider.chat_client()`

## 7. Update `rho-bench/src/harness.rs` ✅

Changes:
- `CountingClient.inner: LocalChatClient` → `Box<dyn ChatClient>`
- `CountingClient::new(inner: LocalChatClient)` → `new(inner: Box<dyn ChatClient>)`
- `run_single_task` takes `&dyn Provider` instead of `&LocalChatClient`
- Constructs `CountingClient::new(provider.clone_boxed_client())`

## 8. Update `rho-bench/src/main.rs` ✅

- `resolve_models()` uses `provider_factory` + `provider.list_models()`
- Accesses `list.data.is_empty()` and `list.data.iter()` (changed from `Vec<ModelInfo>` to `ModelList` return)

## 9. Update `rho-core/tests/integration_tests.rs` ✅

- `test_chat_stream` uses `provider_factory()` + `provider.chat_client()`
- Removed unused `ChatClient` import

## 10. Add deprecation notes to legacy functions ✅

**File:** `rho-core/src/client/mod.rs`

- `client_factory()` doc comment updated with deprecation note pointing to `provider_factory()`
- `is_local_endpoint()` and `resolve_api_key()` remain public (used by `provider_factory` itself)
- No `#[deprecated]` attribute — just doc-level guidance to avoid breaking bench/tests

## 11. Add unit tests ✅

**File:** `rho-core/src/provider.rs` — 8 tests:

| Test | What it validates |
|---|---|
| `provider_new_local` | Localhost endpoint → `!is_external()` |
| `provider_new_external` | Remote endpoint → `is_external()` |
| `provider_new_ipv6_loopback_is_local` | `[::1]` → `!is_external()` |
| `provider_new_ipv6_loopback_bracketed_is_local` | Bracketed IPv6 → local |
| `provider_clone_boxed_client` | Clone doesn't panic |
| `provider_factory_returns_openai_compatible` | Default config → "OpenAI Compatible" |
| `provider_factory_respects_endpoint_override` | CLI `--endpoint` → external |
| `provider_factory_override_beats_config` | CLI > config priority |
| `provider_factory_uses_config_endpoint` | Config endpoint used |
| `provider_factory_respects_api_key_config` | Config `api_key_env` read |
| `provider_factory_api_key_override_beats_config` | CLI > config for API key |

## 12. Fix `doc_markdown` clippy lints ✅

Backticked provider names in doc comments:
- `OpenAI` → `` `OpenAI` ``
- `OpenRouter` → `` `OpenRouter` ``
- `Ollama` → `` `Ollama` ``
- `Anthropic` → `` `Anthropic` ``
- `LM Studio` → `` `LM Studio` ``
- `Groq` → `` `Groq` ``
- `DeepInfra` → `` `DeepInfra` ``

## 13. Run full CI ✅

| Stage | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ Clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ Clean |
| `cargo build --workspace` | ✅ Clean |
| `cargo test --workspace` | ✅ 868 passed, 0 failed, 4 ignored |

---

## Change Summary

| File | Type | Net Lines |
|---|---|---|
| `rho-core/src/provider.rs` | New | +180 |
| `rho-core/src/lib.rs` | Modified | +3 |
| `rho-core/src/client/mod.rs` | Modified | +2 |
| `rho/src/app.rs` | Modified | -30 |
| `rho/src/repl.rs` | Modified | ~0 |
| `rho-bench/src/harness.rs` | Modified | ~0 |
| `rho-bench/src/main.rs` | Modified | ~0 |
| `rho-core/tests/integration_tests.rs` | Modified | ~0 |
