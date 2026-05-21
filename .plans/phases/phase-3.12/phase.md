# Phase 3.12: Multi-Provider Support

**Goal:** Allow rho to be configured with multiple model providers simultaneously — local (LM Studio, Ollama) and external (OpenAI, OpenRouter) — and select models from any of them via a `ProviderRegistry`.

**Milestone (Phase A):** Config supports `[[providers]]` array. `ProviderRegistry` manages multiple providers. Auto-detect queries all providers. Backward-compatible with legacy `[provider]` config. Single consolidated consent prompt for external providers. REPL uses default (first) provider.

**Milestone (Phase B):** `/models` command lists all models across providers. `/model <id>` switches provider and model mid-session without restart.

**Pre-requisite:** Phase 3.11 (Provider Trait) ✅ Complete — `Provider` trait, `OpenAiCompatibleProvider`, `provider_factory()` in place. `App` already holds `Box<dyn Provider>`.

## New Dependencies

None.

## Decisions

**Config format:** TOML array of tables `[[providers]]`, each with `name`, `type`, `endpoint`, `api_key_env`. Legacy `[provider]` single-table format promoted to single-element `[[providers]]` during loading.

**`ProviderConfig` gains `name` field:** Human-readable identifier for display in `/models` listing and for disambiguation (`/model openai/gpt-4o`). Optional — defaults to provider index.

**`ProviderRegistry` lives in `rho-core/src/provider.rs`:** Co-located with the `Provider` trait. Not a separate module — the registry is conceptually part of the provider abstraction.

**Backward compatibility strategy:** Dual-path deserialization. `WireConfig` accepts both `provider = {...}` (legacy) and `[[providers]]` (new). If `providers` is present, use it; otherwise promote `provider` into a single-element vec.

**Consent UX:** Single consolidated prompt listing all external providers, not one prompt per provider. Example: "⚠ External providers detected: OpenRouter, OpenAI. Continue? [y/N]"

**Model conflict resolution (Phase A):** When multiple providers serve the same model ID, config order wins (first match). Phase B adds explicit `provider/model` syntax.

**Bench impact:** None in Phase A. `rho-bench` continues using `provider_factory()` for single-provider construction. Multi-provider bench deferred.

**`provider_factory()` preserved:** Remains public for bench/tests. Gets `#[deprecated]` note pointing to `ProviderRegistry::from_config()`. Does not break existing code.

## Exit Criteria (Phase A)

Users can configure multiple providers in `.rho/config.toml` or `~/.rho/config.toml`. Rho constructs a `ProviderRegistry`, queries all providers for model auto-detection, and selects from any of them. Single-provider users see zero behavior change. External provider consent is shown once for all external providers.

## Exit Criteria (Phase B)

Users can type `/models` to see all available models across providers, and `/model <id>` to switch mid-session. The session's active provider and model update without restart.
