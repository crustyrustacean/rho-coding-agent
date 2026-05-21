# Phase 3.11: Provider Trait

**Goal:** Extract a `Provider` trait to unify scattered provider logic (consent checks, model discovery, externality) currently split between `rho-core/src/client.rs` and `rho/src/app.rs`.

**Milestone:** The `Provider` trait is defined, `OpenAiCompatibleProvider` implements it, and `App` holds `Box<dyn Provider>` instead of a concrete `LocalChatClient`. The `ChatClient` trait remains completely untouched — this is purely additive.

**Current state (Phase 3.11 complete):** `rho-core/src/provider.rs` contains the `Provider` trait (5 methods), `OpenAiCompatibleProvider`, and `provider_factory()`. `App.provider` is `Box<dyn Provider>`. Bench `CountingClient` wraps `Box<dyn ChatClient>` from `provider.clone_boxed_client()`. All 868 tests pass.

**Motivation:** Provider-related logic was scattered across `client.rs` (endpoint construction, API key resolution, externality detection) and `app.rs` (consent prompt, type compatibility check, model resolution). This coupling made it impossible to support multiple providers, test provider behavior in isolation, or build a TUI model picker. The `Provider` trait consolidates all provider-specific behavior behind a uniform interface.

## New Dependencies

None.

## Decisions

**`clone_boxed_client()` on trait:** User chose adding `fn clone_boxed_client(&self) -> Box<dyn ChatClient>` to the `Provider` trait over exposing `inner_client() -> &LocalChatClient`. This keeps bench `CountingClient` generic over `Box<dyn ChatClient>` and doesn't leak the concrete type through the trait API.

**Separate `provider.rs` file:** The provider module is conceptually distinct from the client module. Although small enough to co-locate, a separate file makes the boundary clear and keeps `client/mod.rs` focused on the `ChatClient` trait and `LocalChatClient`.

**`list_models()` returns `Result<ModelList>`:** Initially returned `Result<Vec<ModelInfo>>` to decouple from the OpenAI wire format, but changed to `Result<ModelList>` per user request to preserve top-level fields (e.g. future pagination metadata).

**Drop path-endpoint compatibility check:** The old `check_provider_compatibility()` had two checks: a URL path check (looking for `/v1/chat/completions`) and a type-label check (warning for known non-OpenAI providers). The path check was dropped — let runtime errors speak for themselves. Only the type-label warning was kept.

**Backward compatibility:** `client_factory()`, `is_local_endpoint()`, and `resolve_api_key()` remain public and functional. Only a deprecation note was added to `client_factory()` doc comment. Bench and tests continue using `client_factory()` without changes (though `provider_factory()` is the recommended replacement).

**Scope:** `ChatClient` trait remains completely unchanged. All downstream code (agent loop, session, tools, tests) continues to use `&dyn ChatClient`. The refactoring is purely additive — no existing behavior was modified.

## Exit Criteria

The `Provider` trait provides a uniform abstraction for provider identity, externality, model discovery, and chat client access. `App` is decoupled from `LocalChatClient`. Bench wraps providers generically. The foundation is laid for `ProviderRegistry` (Phase 3.12) and non-OpenAI provider implementations.
