# Phase 3.11: Provider Trait

## Overview

**Goal:** Introduce a `Provider` trait to unify scattered provider logic (consent checks, model discovery, externality) into a single abstraction, decoupling the app from the concrete `LocalChatClient`.

**Status:** ✅ Complete

**Estimated Effort:** 15–20 hours (actual)

## Motivation

Provider-related logic was split between `rho-core/src/client.rs` (endpoint construction, API key resolution, externality check) and `rho/src/app.rs` (consent prompt, provider type warning, model resolution). This made it impossible to:

- Support multiple providers without duplicating setup logic
- Test provider behavior in isolation from the app
- Add a TUI model picker (needs a uniform interface to query models)

The `Provider` trait encapsulates identity, externality, model discovery, and chat client access behind a single interface. The agent loop, session, and tools remain completely unaware of providers — they only see `&dyn ChatClient`.

## Key Files

- **Implementation:** `rho-core/src/provider.rs` (new)
- **Updated:** `rho-core/src/client/mod.rs`, `rho-core/src/lib.rs`, `rho/src/app.rs`, `rho/src/repl.rs`, `rho-bench/src/harness.rs`, `rho-bench/src/main.rs`, `rho-core/tests/integration_tests.rs`
- **Research note:** Obsidian `Rust Projects/rho-coding-agent/4. Research/Provider Trait Exploration.md`

## What Changed

### New: `rho-core/src/provider.rs`

- **`Provider` trait** — `name()`, `is_external()`, `list_models() → Result<ModelList>`, `chat_client() → &dyn ChatClient`, `clone_boxed_client() → Box<dyn ChatClient>`
- **`OpenAiCompatibleProvider`** — wraps `LocalChatClient`, derives `is_external` from endpoint URL, returns full `ModelList`, implements `clone_boxed_client` via `LocalChatClient::clone()`
- **`provider_factory()`** — mirrors `client_factory()` but returns `Box<dyn Provider>`
- **8 unit tests** covering construction, externality, factory config resolution

### Modified: `rho-core/src/lib.rs`

- Added `pub mod provider` declaration
- Added `pub use provider::{OpenAiCompatibleProvider, Provider, provider_factory}` re-exports

### Modified: `rho-core/src/client/mod.rs`

- Added deprecation note to `client_factory()` doc comment (still available for bench/tests)

### Modified: `rho/src/app.rs`

- `App.client: LocalChatClient` → `App.provider: Box<dyn Provider>`
- Merged old steps 4+5+9 into single step 4 (`provider_factory`)
- Replaced `check_provider_compatibility()` + `resolve_endpoint()` with simpler `check_provider_type()`
- `check_provider_consent()` takes `&dyn Provider` (uses `provider.is_external()` and `provider.name()`)
- `resolve_model()` takes `&dyn Provider` (uses `provider.list_models()`)
- Consent prompt shows provider name instead of raw endpoint URL
- **Net: ~30 lines removed**

### Modified: `rho/src/repl.rs`

- `&app.client` → `app.provider.chat_client()` (2 call sites)

### Modified: `rho-bench/src/harness.rs`

- `CountingClient.inner: LocalChatClient` → `Box<dyn ChatClient>`
- `CountingClient::new(inner: LocalChatClient)` → `new(inner: Box<dyn ChatClient>)`
- `run_single_task(client: &LocalChatClient)` → `(provider: &dyn Provider)`
- Uses `provider.clone_boxed_client()` to create the `CountingClient`

### Modified: `rho-bench/src/main.rs`

- `resolve_models()` uses `provider_factory` + `provider.list_models()` → `Vec<ModelInfo>`

### Modified: `rho-core/tests/integration_tests.rs`

- `test_chat_stream` updated to use `provider_factory` + `provider.chat_client()`
- Removed unused `client_factory` import

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| `clone_boxed_client()` vs `inner_client()` | `clone_boxed_client()` on trait | Keeps bench `CountingClient` generic over `Box<dyn ChatClient>`; doesn't leak concrete type |
| Co-locate trait in `client/mod.rs` or `provider.rs` | `provider.rs` (separate file) | Module was growing; provider concept is distinct from client |
| `list_models()` return type | `Result<ModelList>` | Preserves top-level fields (e.g. future pagination); was initially `Vec<ModelInfo>` then changed per user request |
| Path-endpoint compatibility check | Dropped | Let runtime errors speak; only warn on known non-OpenAI `type` labels |
| `client_factory()` deprecated? | Yes, doc-level deprecation | Still available for bench/tests; `provider_factory()` is the recommended replacement |

## Success Criteria

1. ✅ `Provider` trait defined with 5 methods
2. ✅ `OpenAiCompatibleProvider` implements `Provider`
3. ✅ `provider_factory()` constructs from `RhoConfig` + CLI overrides
4. ✅ `App` holds `Box<dyn Provider>` instead of `LocalChatClient`
5. ✅ `ChatClient` trait completely untouched — all downstream code unchanged
6. ✅ `rho-bench` uses `provider.clone_boxed_client()` for `CountingClient`
7. ✅ All 868 tests pass, 0 failures
8. ✅ fmt, clippy, build all clean

## CI Validation

| Stage | Result |
|---|---|
| `cargo fmt --all -- --check` | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo build --workspace` | ✅ clean |
| `cargo test --workspace` | ✅ 868 passed, 0 failed, 4 ignored |

## Foundation For

- **Phase 3.12:** `ProviderRegistry` manages multiple `Box<dyn Provider>` instances
- **Phase 4 TUI:** Model picker queries `Provider::list_models()` across all providers
- **Non-OpenAI providers:** Each new provider (Anthropic, Gemini) is a new `impl Provider`
