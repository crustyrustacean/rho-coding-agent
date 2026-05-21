# Phase 3.12: Multi-Provider Support

## Overview

**Goal:** Allow rho to be configured with multiple model providers simultaneously — a local server (LM Studio / Ollama) plus one or more external providers (OpenAI, OpenRouter) — and select models from any of them.

**Status:** 🔜 In Planning

**Estimated Effort:** Phase A: 15–25 hours | Phase B: 10–15 hours

## Motivation

Currently rho supports exactly one provider endpoint. Users who run local models alongside cloud APIs (OpenRouter, OpenAI) must manually reconfigure and restart to switch. Multi-provider support enables:

- **Unified model selection:** `/model sonnet` picks from any configured provider
- **Seamless fallback:** Try local first, fall back to cloud when the model isn't loaded locally
- **Side-by-side comparison:** Bench the same task against local and remote models in one session
- **Foundation for TUI model picker:** The provider registry is the data source for any future UI

## Pre-requisite: Phase 3.11 (Provider Trait) ✅ Complete

Phase 3.11 introduced the `Provider` trait (`rho-core/src/provider.rs`), `OpenAiCompatibleProvider`, and `provider_factory()`. It also refactored `App` to hold `Box<dyn Provider>` instead of a concrete `LocalChatClient`. This phase builds directly on that abstraction — no further trait changes are expected.

## Key Files

- **Implementation plan:** `phase.md`
- **Detailed tasks:** `tasks.md`
- **Readiness assessment:** `readiness.md`
- **Source code touched:** `rho-core/src/provider.rs`, `rho-core/src/config.rs`, `rho-core/src/lib.rs`, `rho/src/app.rs`, `rho/src/repl.rs`, `rho/src/cli.rs`

## Two-Phase Delivery

### Phase A — Provider Registry & Multi-Provider Config

Config supports `[[providers]]` array. A `ProviderRegistry` holds multiple `Box<dyn Provider>`. Auto-detect queries all providers. The REPL uses the default (first) provider. Backward-compatible with existing `[provider]` config.

### Phase B — In-Session Model Switching (next PR)

`/models` lists all models across providers. `/model <id>` switches provider and model mid-session. `App` tracks the active provider index. This is the main user-facing payoff.

## Architecture

### Config Format (Phase A)

```toml
# New multi-provider format
[[providers]]
name = "local"
endpoint = "http://localhost:1234/v1/chat/completions"

[[providers]]
name = "openrouter"
type = "openrouter"
endpoint = "https://openrouter.ai/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"

[[providers]]
name = "openai"
type = "openai"
endpoint = "https://api.openai.com/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
```

Legacy `[provider]` format continues to work (promoted to a single-element `[[providers]]`).

### Module Layout

```
rho-core/src/
├── provider.rs    # Provider trait + OpenAiCompatibleProvider (existing)
│                  # + ProviderRegistry (new in this phase)
├── config.rs      # ProviderSettings wrapping Vec<ProviderConfig> (modified)
└── lib.rs         # Re-exports (updated)

rho/src/
├── app.rs         # App.registry: ProviderRegistry (modified)
├── cli.rs         # --endpoint/--api-key-env override default provider (unchanged)
└── repl.rs        # app.registry.default().chat_client() (Phase A)
                   # app.active_provider().chat_client() (Phase B)
```

## Success Criteria (Phase A)

1. ✅ Config supports `[[providers]]` array with name, endpoint, api_key_env
2. ✅ Legacy `[provider]` config transparently promoted to single-element array
3. ✅ `ProviderRegistry` constructed from config with `from_config()`
4. ✅ `resolve_model()` queries all providers when no explicit model set
5. ✅ External provider consent checked for all external providers (single prompt)
6. ✅ `--endpoint` and `--api-key-env` override the default (first) provider
7. ✅ All existing tests pass, new registry tests added
8. ✅ `rho-bench` continues to work unchanged (single-provider `provider_factory`)
9. ✅ Backward compatible: zero behavior change for single-provider users

## Success Criteria (Phase B)

1. ✅ `/models` lists all models across providers with provider name prefix
2. ✅ `/model <id>` fuzzy-matches and switches provider + model
3. ✅ `/model <provider>/<id>` selects explicit provider
4. ✅ Session model updated in-place (no restart)
5. ✅ Ambiguous model IDs handled with disambiguation prompt

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Config migration breaks existing users | Low | High | Legacy `[provider]` → `[[providers]]` promotion with tests |
| Consent UX annoying with 3+ external providers | Medium | Low | Single consolidated consent prompt |
| Ambiguous model IDs across providers | Medium | Medium | Config order for auto; explicit `provider/model` for manual |
| `rho-bench` needs multi-provider support | Low | Low | Defer — bench targets single endpoint via `provider_factory` |

## Open Questions (to resolve before implementation)

1. **Default model selection** — When multiple providers and no `--model`: (a) fail with message, (b) prefer local, (c) pick first available?
2. **Phase B timing** — Implement `/model` switching now or defer to separate PR?
3. **Bench multi-provider** — Defer to separate PR?
