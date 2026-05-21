# Phase 3.12 Readiness Assessment

**Date:** 2026-05-21 | **Phase:** 3.12 - Multi-Provider Support | **Status:** 🔜 Ready to Start (pending open questions)

---

## Executive Summary

**Ready to start Phase A development**, pending resolution of open questions about default model selection behavior. All prerequisites are in place: the `Provider` trait is implemented and wired through the app, config infrastructure is mature, and the change surface is well-understood.

---

## ✅ What's Ready

### Provider Trait — Complete (Phase 3.11)

The `Provider` trait (`rho-core/src/provider.rs`) provides exactly the abstraction needed:
- `name()` — display string for consent prompts and `/models` listing
- `is_external()` — externality check for consent gating
- `list_models() → Result<ModelList>` — model discovery per provider
- `chat_client() → &dyn ChatClient` — agent loop access
- `clone_boxed_client() → Box<dyn ChatClient>` — owned client for bench wrapping

`OpenAiCompatibleProvider` wraps `LocalChatClient` with externality derived from the endpoint URL. This covers local servers and all OpenAI-compatible external providers (OpenRouter, OpenAI, Groq, DeepInfra).

### Config Infrastructure — Mature

`ConfigLoader` supports two-tier merging (user + project) with `WireConfig` deserialization. The existing `ProviderConfig` struct has `type`, `endpoint`, `api_key_env` fields. Adding a `name` field and wrapping in a `Vec` is a straightforward extension.

The TOML array-of-tables format `[[providers]]` is native to `toml`/`serde` — no special handling needed for deserialization.

### App Assembly — Clean Entry Point

`App::build()` runs 15 numbered setup phases. Phase 4 constructs the provider and phase 6 checks consent. Both are isolated private functions — replacing `Box<dyn Provider>` with `ProviderRegistry` is localized to `app.rs`.

### REPL — Minimal Coupling

`repl.rs` accesses the provider through `app.provider.chat_client()` in exactly two places (`run_repl` and `run_prompt_file`). Changing to `app.registry.default().chat_client()` is a one-line change per call site.

### Bench — Isolated

`rho-bench` builds its own provider via `provider_factory()`, completely independent of the app's provider setup. No changes needed in Phase A.

### Test Infrastructure — Solid

- Unit tests in `rho-core/src/provider.rs` (8 tests for `OpenAiCompatibleProvider` and `provider_factory`)
- Integration tests in `rho-core/tests/` (1 call site using `provider_factory`)
- `rho-test-helpers` does not reference `provider_factory` or `client_factory`
- Config tests in `rho-core/src/config.rs` (20+ tests for `ConfigLoader`, merging, API keys)

### Backward Compatibility — Well-Understood

The only user-facing config change is `[provider]` → `[[providers]]`. The dual-path deserializer ensures existing configs continue to work. All CLI flags (`--endpoint`, `--api-key-env`, `--model`, `--accept-external-provider`) remain unchanged.

---

## ⚠️ Open Questions

### 1. Default Model Selection (affects Phase A)

When multiple providers are configured and the user doesn't specify `--model` or `agent.model`:

- **(a) Fail with message** — "Multiple providers configured; specify --model or /model"
  - Pros: Explicit, no surprising auto-selection
  - Cons: Breaks the current zero-config UX for single-provider users who add a second provider
- **(b) Prefer local, fall back to first external** — Query local first; if it has models, use the first one; otherwise try external providers in order
  - Pros: Matches intuition (use local when possible), maintains zero-config UX
  - Cons: Silently picks a model the user might not want
- **(c) Pick first available** — Query all providers in config order, use first model found
  - Pros: Simplest implementation
  - Cons: Might pick an expensive cloud model when a local one is available

**Recommendation:** Option (c) for Phase A (simplest, matches config order intent), with a log message showing which provider/model was selected. Phase B `/models` makes this fully transparent.

### 2. Phase B Timing

Should `/model` and `/models` be in the same PR as the registry?

- **Same PR:** More coherent deliverable, but larger diff
- **Separate PR:** Smaller reviewable chunks, but leaves Phase A without the main user-facing feature

**Recommendation:** Separate PR. Phase A is self-contained and valuable (config-driven multi-provider). Phase B adds the interactive switching.

### 3. Bench Multi-Provider (deferred)

Should `rho-bench` support `--provider local --provider openrouter` to run tasks against multiple endpoints?

**Recommendation:** Defer. Bench already targets a single endpoint via `--endpoint`. Multi-provider bench is a distinct use case.

---

## 🔴 Pre-Work Items

1. **Resolve open question 1** (default model selection strategy)
2. Review this plan with stakeholder

---

## Task Path

### Phase A Tasks (this PR)

| Order | Task | Rationale |
|---|---|---|
| 1 | Extend `ProviderConfig` with `name` field | Foundation — every provider needs a display name |
| 2 | Create `ProviderSettings` wrapper with `Vec<ProviderConfig>` | Config schema for multi-provider |
| 3 | Add dual-path `WireConfig` deserialization | Backward compat with legacy `[provider]` |
| 4 | Update `ConfigLoader::merge()` for `ProviderSettings` | Two-tier merge for provider arrays |
| 5 | Implement `ProviderRegistry` | Core abstraction for multi-provider |
| 6 | Add `ProviderRegistry::from_config()` | Config-driven construction |
| 7 | Add `ProviderRegistry::list_all_models()` | Cross-provider model discovery |
| 8 | Update `App` to hold `ProviderRegistry` | Replace `Box<dyn Provider>` |
| 9 | Update `resolve_model()` for multi-provider | Auto-detect across all providers |
| 10 | Consolidate external provider consent | Single prompt for all externals |
| 11 | Update `repl.rs` call sites | `registry.default().chat_client()` |
| 12 | Add `ProviderRegistry` unit tests | Construction, lookup, model listing |
| 13 | Add config tests for `[[providers]]` | New format, legacy compat, merging |
| 14 | Deprecate `provider_factory()` | Point to `ProviderRegistry::from_config()` |
| 15 | Run full CI | fmt, clippy, build, test |

### Phase B Tasks (next PR)

| Order | Task |
|---|---|
| 1 | Add `App.active_provider_index` tracking |
| 2 | Implement `/models` REPL command |
| 3 | Implement `/model <id>` with fuzzy matching |
| 4 | Implement `/model <provider>/<id>` explicit selection |
| 5 | Session model/provider update integration |
| 6 | Tests for model switching |

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Config migration breaks existing users | Low | High | Dual-path deserializer with comprehensive tests |
| Consent UX annoying with 3+ external providers | Medium | Low | Single consolidated prompt listing all names |
| Ambiguous model IDs across providers | Medium | Medium | Config order for auto; explicit `provider/model` for manual |
| Auto-detect selects expensive cloud model | Medium | Low | Log which provider/model was selected; user can override with `--model` |
| `provider_factory()` deprecation causes warnings | Low | Low | `#[deprecated]` with clear migration path; bench can suppress |

---

## Dependencies

**Modified crates:**
- `rho-core` (`provider.rs`, `config.rs`, `lib.rs`)
- `rho` (`app.rs`, `repl.rs`)

**No changes to:**
- `rho-bench` (Phase A)
- `rho-tools`
- `rho-eval`
- `rho-test-helpers`
- `rho-highlight`

---

## Estimated Effort

| Task | Estimated Effort |
|------|------------------|
| Config schema changes | 2–3 hours |
| `ProviderRegistry` implementation | 3–4 hours |
| `App` refactoring | 2–3 hours |
| Consent consolidation | 1 hour |
| REPL updates | 1 hour |
| Tests (config + registry) | 3–4 hours |
| CI validation | 1 hour |
| **Phase A Total** | **13–17 hours** |

**Phase B estimated:** 10–15 hours (not included above).

---

## Blockers

None. Awaiting resolution of open question 1 (default model selection).

---

## Next Steps

1. Resolve open question 1 (default model selection strategy)
2. Confirm Phase B separate PR decision
3. Begin Task 1: Extend `ProviderConfig` with `name` field
4. Proceed through Phase A tasks sequentially

---

**Last updated:** 2026-05-21
