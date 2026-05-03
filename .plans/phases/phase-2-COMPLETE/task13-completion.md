# Task 13: Configurable Token Budget — Completion Report

**Date:** 2026-05-01 (retroactive)
**Branch:** Commit `f0d4d96`
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Made the token budget configurable via config and CLI, and raised the default from 8,192 to 32,768 tokens.

### 1. Config-driven token budget

**`rho-core/src/config.rs`:**
- Added `token_budget: u32` field to `AgentLoopConfig` with default of 32,768
- Added `token_budget: Option<u32>` to `WireAgentLoopConfig` for two-tier merge
- Merge logic: project overrides user, default 32,768 when neither sets it
- TOML: `[agent] token_budget = 32768`

### 2. CLI flag

**`rho/src/main.rs`:**
- Added `--token-budget <N>` CLI flag that overrides the config value
- Priority: CLI flag → config value → default (32,768)
- Wired into `Conversation::with_token_budget()`

### 3. Default budget raised

**`rho-core/src/context.rs`:**
- `TokenBudget::default()` changed from `8_192` to `32_768`
- The original 8K default was a Phase 1a placeholder that left only ~3.5K tokens for conversation after the system prompt, insufficient for even 1–2 tool-call rounds

### 4. Code organization in binary

- Extracted `load_system_prompt()` and `check_provider_consent()` helpers from `main()` to satisfy `clippy::too_many_lines`

---

## Test Coverage

### New unit tests in `config.rs` (7 tests)

| Test | What it verifies |
|---|---|
| `token_budget_defaults_to_32k` | Default `AgentLoopConfig` has 32,768 |
| `token_budget_from_config` | TOML value parsed correctly |
| `token_budget_project_overrides_user` | Project value wins over user value |
| `token_budget_user_preserved_when_no_project` | User value applies when project doesn't set it |
| `default_is_32k` | `default_token_budget()` returns 32,768 |

### New unit test in `context.rs` (1 test)

| Test | What it verifies |
|---|---|
| `default_token_budget_is_32k` | `TokenBudget::default()` is 32,768 |

### New integration tests (2 tests)

| Test | What it verifies |
|---|---|
| `conversation_default_token_budget` | `Conversation::new()` uses 32K default |
| `custom_token_budget_evicts_old_messages` | Small budget triggers eviction of older turns |

### Test count progression

| Suite | Before | After |
|---|---|---|
| rho-core unit | 90 | 98 (+8) |
| rho-core integration | 32 | 34 (+2) |
| **Total** | **270** | **277 (+7)** |

---

## File Changes

| File | Change |
|---|---|
| `rho-core/src/config.rs` | Added `token_budget` field to `AgentLoopConfig` and `WireAgentLoopConfig`, merge logic, 7 unit tests |
| `rho-core/src/context.rs` | Changed `TokenBudget::default()` from 8,192 to 32,768, added 1 unit test |
| `rho-core/tests/integration_tests.rs` | Added 2 integration tests |
| `rho/src/main.rs` | Added `--token-budget` CLI flag, extracted helpers, wired budget into conversation |

---

## Design Decisions

1. **32K default** — The Phase 1a default of 8K left only ~3.5K tokens for conversation after the system prompt (~4.5K tokens with base.md + tool schemas). Mid-size models (qwen3-14B) can't self-correct when they only have room for 1–2 turns. 32K leaves ~27K tokens for conversation, sufficient for multi-turn tool use with most models.

2. **CLI overrides config** — The `--token-budget` flag takes precedence over the config value. This allows quick experimentation without editing config files.

3. **`u32` not `usize`** — Token counts are stored as `u32` in config (TOML-friendly). Converted to `usize` at the `TokenBudget` boundary. This keeps config serialization simple and consistent.

4. **Context manager strategy unchanged** — This task only changes the budget *value*, not the eviction strategy. `SlidingWindowContextManager` still evicts oldest turns first when the budget is exceeded. Sophisticated strategies (summarisation, retrieval-augmented) remain a Phase 5+ concern.

5. **Helper extraction satisfies clippy** — `main()` exceeded the `clippy::too_many_lines` limit after adding the token budget wiring. Extracting `load_system_prompt()` and `check_provider_consent()` into named helpers improves readability and satisfies the lint.
