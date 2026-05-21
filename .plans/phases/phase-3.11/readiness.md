# Phase 3.11 Readiness Assessment

**Date:** 2026-05-21 (retroactive) | **Phase:** 3.11 - Provider Trait | **Status:** ✅ Complete

---

## Executive Summary

Phase 3.11 introduced the `Provider` trait to unify provider-specific logic into a single abstraction. The refactoring was purely additive — the `ChatClient` trait was untouched, and all downstream code continued to work via `&dyn ChatClient`. The work is complete and validated (868 tests, 0 failures).

---

## What Was Done

### Before

Provider logic was scattered:
- **`rho-core/src/client.rs`:** `client_factory()`, `is_local_endpoint()`, `resolve_api_key()`, `LocalChatClient::list_models()`
- **`rho/src/app.rs`:** `resolve_endpoint()`, `check_provider_compatibility()`, `check_provider_consent()`, `resolve_model()` — all tightly coupled to `LocalChatClient`
- **`rho-bench/src/harness.rs`:** `CountingClient` wrapped `LocalChatClient` directly

No way to represent "a provider" as a first-class concept. No path to multi-provider support without duplicating setup logic.

### After

- **`rho-core/src/provider.rs`** (new): `Provider` trait, `OpenAiCompatibleProvider`, `provider_factory()`
- **`App.provider: Box<dyn Provider>`** — replaces `App.client: LocalChatClient`
- **`CountingClient.inner: Box<dyn ChatClient>`** — replaces `LocalChatClient`
- Consolidated consent and model resolution behind trait methods

---

## Pre-requisites Met

| Prerequisite | Status |
|---|---|
| `ChatClient` trait stable | ✅ No changes made |
| `LocalChatClient` is `Clone` | ✅ Enables `clone_boxed_client()` |
| `ModelList` / `ModelInfo` types defined | ✅ Used as-is |
| Config system supports `provider.endpoint`, `provider.api_key_env` | ✅ Used by `provider_factory()` |
| Bench `CountingClient` testable | ✅ Now generic over `Box<dyn ChatClient>` |

---

## Decisions Made During Implementation

### Open Question 1: `clone_boxed_client()` vs `inner_client()`

**Question:** Should the trait expose `inner_client() -> &LocalChatClient` (simple but leaks concrete type) or `clone_boxed_client() -> Box<dyn ChatClient>` (keeps abstraction)?

**Decision:** `clone_boxed_client()`. User's explicit answer. Keeps bench `CountingClient` generic. Concrete providers must implement cloning, which is feasible because `LocalChatClient` is `Clone`.

### Open Question 2: Co-location vs separate file

**Question:** Put the trait in `client/mod.rs` (small module, shared internals) or a new `provider.rs`?

**Decision:** New `provider.rs`. Conceptually distinct from the client module. `OpenAiCompatibleProvider` shares `SseStream`/wire-type internals but the trait itself is a higher-level abstraction.

### Open Question 3: `list_models()` return type

**Question:** Return `Result<Vec<ModelInfo>>` (decoupled from wire format) or `Result<ModelList>` (preserves top-level fields)?

**Decision:** Initially `Vec<ModelInfo>`, then changed to `ModelList` per user request. Top-level fields (pagination, metadata) may be needed for non-OpenAI providers.

### Open Question 4: Compatibility check scope

**Question:** Keep the URL path-endpoint check (`/v1/chat/completions`) or drop it?

**Decision:** Drop it. Let runtime HTTP errors speak for themselves. Only keep the `type`-label warning for known non-OpenAI providers.

### Open Question 5: Deprecation strategy

**Question:** Remove `client_factory()`, `is_local_endpoint()`, `resolve_api_key()` or keep them?

**Decision:** Keep as public API with deprecation doc comments. Bench and integration tests still use them. Removal deferred to a future breaking-change release.

---

## Code Changes Summary

| File | Change Type | Lines Changed |
|---|---|---|
| `rho-core/src/provider.rs` | New file | ~180 lines (trait + impl + factory + 8 tests) |
| `rho-core/src/lib.rs` | Modified | +3 (mod + re-exports) |
| `rho-core/src/client/mod.rs` | Modified | +2 (deprecation note in doc comment) |
| `rho/src/app.rs` | Modified | Net ~-30 (simplified setup, deleted `resolve_endpoint`) |
| `rho/src/repl.rs` | Modified | ~2 (`.chat_client()` call sites) |
| `rho-bench/src/harness.rs` | Modified | ~10 (generic `Box<dyn ChatClient>`) |
| `rho-bench/src/main.rs` | Modified | ~5 (`provider_factory` + `.data` access) |
| `rho-core/tests/integration_tests.rs` | Modified | ~3 (swap `client_factory` → `provider_factory`) |

**Net change:** ~+170 lines (mostly new `provider.rs`). No deletions of functional code.

---

## Test Results

| Suite | Result |
|---|---|
| Unit tests | ✅ All pass (including 8 new provider tests) |
| Integration tests | ✅ All pass |
| Doc tests | ✅ 10 pass, 1 ignored (shell-dependent) |
| Bench | ✅ Builds and runs |
| `cargo fmt` | ✅ Clean |
| `cargo clippy` | ✅ Clean (fixed 2 `doc_markdown` lints) |

---

## Foundation For

- **Phase 3.12:** `ProviderRegistry` holds `Vec<Box<dyn Provider>>` — each entry is already a `dyn Provider`
- **Non-OpenAI providers:** New `impl Provider` for Anthropic, Gemini, etc. — agent loop doesn't change
- **TUI model picker:** Queries `Provider::list_models()` across all providers in the registry
- **Rich `ModelInfo`:** Extending `ModelInfo` fields is independent of the trait API

---

**Last updated:** 2026-05-21 (retroactive)
