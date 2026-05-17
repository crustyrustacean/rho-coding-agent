# Phase 3.4 Tasks ✅ ALL COMPLETE

**Completed:** 2026-05-14 | **Version:** 0.33.x

---

### Task 3.4.0: Shared Bootstrapping in `rho-core` ✅

- [x] Create `client_factory()` in `rho-core/src/client.rs`
- [x] Create `resolve_api_key()` in `rho-core/src/client.rs`
- [x] Create `compose_full_system_prompt()` in `rho-core/src/context_files.rs`
- [x] Re-export both functions from `rho-core/src/lib.rs`
- [x] Refactor `rho/src/main.rs` to use shared functions
- [x] Refactor `rho-bench/src/harness.rs` to use shared functions

### Task 3.4.1: CLI Flags for Provider Configuration ✅

- [x] Add `--endpoint` flag to `rho` CLI
- [x] Add `--api-key-env` flag to `rho` CLI
- [x] Add `--max-iterations` flag to `rho` CLI
- [x] Pass overrides through to `client_factory()` and `compose_full_system_prompt()`
- [x] `--endpoint` implies `--accept-external-provider`

### Task 3.4.2: Egress Enforcement in `rho-bench` ✅

- [x] `rho-bench` loads full `RhoConfig` via `ConfigLoader`
- [x] Passes config through to `client_factory()` — egress enforcement comes for free

### Task 3.4.3: Robust Evaluation Scenario Library 🔄 (Ongoing)

- [ ] Additional `EvalTask` implementations beyond the current 5
- Priority categories: compiler errors (E0425, E0382, E0616), test failures, new code creation, Cargo project setup

### ⏸️ Task 3.4.4: Named Provider Presets (deprioritized)

- Not implemented — CLI flags cover the immediate need
