# Phase 3.4: First-Class Frontier Model Support ✅ COMPLETE

**Goal:** Make frontier models (via OpenRouter or any OpenAI-compatible provider) a first-class workflow alongside local models. Both `rho` and `rho-bench` should be able to target local or remote with a single flag change.

**Milestone:** Shared bootstrapping (`client_factory`, `compose_full_system_prompt`) replaces divergent inline logic in `rho` and `rho-bench`. CLI flags (`--endpoint`, `--api-key-env`, `--max-iterations`) allow provider switching without editing config.

**Current state (pre-Phase 3.4):** `rho` and `rho-bench` have evolved as parallel entry points with independent bootstrapping logic. They don't share config loading, client construction, system prompt composition, or agent configuration. `rho-bench` was not testing the same code paths as production `rho`.

## Implementation

### 3.4.0 — Shared Bootstrapping in `rho-core` ✅

Extracted shared bootstrap functions into `rho-core` that both `rho` and `rho-bench` call:

**New public API:**
- `client_factory()` in `rho-core/src/client.rs` — constructs a fully-configured `LocalChatClient` from `RhoConfig` with CLI overrides
- `compose_full_system_prompt()` in `rho-core/src/context_files.rs` — builds the complete system prompt from base prompt, context files, trust store, and environment info

**What was shared:**
| Piece | From | To |
|---|---|---|
| `client_factory()` + `resolve_api_key()` | `main.rs` | `rho-core/src/client.rs` |
| `compose_full_system_prompt()` | `main.rs` (~40 lines) | `rho-core/src/context_files.rs` |
| Environment info block | Both `main.rs` and `harness.rs` | Inside `compose_full_system_prompt()` |
| Rust tooling guidance block | Both `main.rs` and `harness.rs` | Inside `compose_full_system_prompt()` |

**Outcome:** Both binaries load config the same way, build the same prompt, construct the same client with egress enforcement, and use the same agent config. `rho-bench` is now a thin orchestration shell over shared code paths.

### 3.4.1 — CLI Flags for Provider Configuration ✅

Added `--endpoint`, `--api-key-env`, and `--max-iterations` flags to `rho`'s CLI with priority: CLI flag → config → auto-detect.

### 3.4.2 — Egress Enforcement in `rho-bench` ✅

With 3.4.0 in place, `rho-bench` calls `client_factory()` which always includes egress enforcement from config. No separate changes needed.

### 3.4.3 — Robust Evaluation Scenario Library 🔄 (ongoing)

The `EvalTask` trait architecture is sound. 5 scenarios implemented. Additional scenarios are added as needed based on model weaknesses discovered in practice.

### ⏸️ 3.4.4 — Named Provider Presets (deprioritized)

CLI flags from 3.4.1 cover the immediate need. Presets deferred until users regularly switch between 3+ providers.

## Key Design Decisions

- `--endpoint` implies `--accept-external-provider` (user explicitly set it, consent implied)
- Shared functions accept CLI overrides directly, minimizing diff in both binaries
- Functions live in existing files (`client.rs`, `context_files.rs`) rather than a new `bootstrap.rs` module

## Exit Criteria ✅

- [x] `client_factory()` exists in `rho-core` and is used by both binaries
- [x] `compose_full_system_prompt()` exists in `rho-core` and is used by both binaries
- [x] `rho` has `--endpoint`, `--api-key-env`, `--max-iterations` CLI flags
- [x] `rho-bench` uses shared bootstrapping (same config, prompt, client as `rho`)
- [x] Egress enforcement works in `rho-bench` (implicit from shared bootstrapping)
- [x] All existing tests pass unchanged
