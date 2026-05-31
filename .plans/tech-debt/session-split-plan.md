# Session.rs God Object Split Plan — TDD Approach

**Date:** 2026-05-31
**Scope:** `rho-core/src/session.rs` (3,943 lines, 56 methods, 12 fields, 9 responsibilities)
**Target:** Extract methods from `session.rs` into focused submodules within `session/`, keeping the external API identical.
**Constraint:** Use new Rust module style (`session.rs` is the module root, submodules live in `session/`). All existing callers compile unchanged. Zero test regressions.

**Note:** The codebase already uses the correct new-style layout — `session.rs` is the module root and `session/` contains submodules. The refactoring moves code *out of* `session.rs` into new files within `session/`.

---

## Architecture

### Current State

```
session.rs                    # 3,943 lines, 56 methods, 103 tests
session/
  entry.rs                   # Entry, EntryPayload, EntryResolution, CompactionSummary (21 tests)
  error.rs                   # SessionError (7 tests)
  estimator.rs               # TokenEstimator, HeuristicEstimator (14 tests)
  compaction.rs              # CompactionStrategy, MechanicalCompactionStrategy (7+11 tests)
  persist.rs                 # PersistState, JSONL persistence, session discovery (6 tests)
```

### Target State

```
session.rs                    # Module root: Session struct, Debug impl, pub mod declarations, re-exports
session/
  entry.rs                   # (unchanged) Entry types, CompactionSummary
  error.rs                   # (unchanged) SessionError
  estimator.rs               # (unchanged) TokenEstimator, HeuristicEstimator
  compaction.rs              # (unchanged) CompactionStrategy
  persist.rs                 # (unchanged) JSONL persistence
  builder.rs                 # NEW — Session::new, Session::in_memory, builder methods
  accessors.rs               # NEW — read-only accessors (header, leaf, entry, model, budget, etc.)
  navigation.rs              # NEW — path_to_root, children, branch_to, branch_with_summary
  append.rs                  # NEW — append_user_message, append_assistant_message, append_tool_result,
                              #       append_compaction, append_branch_summary, append_label,
                              #       append_custom_state, append_custom_message, close
  context.rs                 # NEW — path_messages, send_current, context_stats
  extensions.rs              # NEW — ExtensionEntry, ExtensionMessageEntry traits,
                              #       write/read_custom_state, write/read_custom_message
  truncation.rs              # NEW — tool-result truncation helpers (private, used by append.rs)
  context_stats.rs           # NEW — ContextStats struct
  header.rs                  # NEW — SessionHeader struct
  tree.rs                    # NEW — internal tree operations (append_entry, estimate helpers)
```

### Module Dependency Graph

```
session.rs (Session struct definition + pub mod declarations + re-exports)
  ├── builder.rs     (construction)
  ├── accessors.rs   (read-only queries)
  ├── navigation.rs  (branch_to, path_to_root, children)
  ├── append.rs      (all append operations)
  │   └── truncation.rs  (private truncation helpers)
  ├── context.rs     (path_messages, send_current)
  ├── extensions.rs  (typed extension entry read/write)
  ├── context_stats.rs (ContextStats struct)
  └── header.rs      (SessionHeader struct)

Internal helpers:
  ├── tree.rs        (append_entry core, format_message_text, etc.)
  ├── compaction.rs  (unchanged, called from context.rs compact_older_than)
  ├── persist.rs     (unchanged, flush_session called from tree.rs)
  ├── entry.rs       (unchanged, types used everywhere)
  ├── error.rs       (unchanged)
  └── estimator.rs   (unchanged)
```

### Key Design Decision: `impl` Blocks in Submodule Files

Each submodule will contain `impl Session` blocks. Rust allows split `impl` blocks across files within the same module. The Session struct definition stays in `session.rs` (the module root), and each submodule adds methods via separate `impl Session` blocks. All methods remain `pub` on `Session` — callers see zero change.

This is the standard Rust pattern (used by `std` itself — e.g., `impl Vec<T>` is spread across multiple files).

---

## Method Allocation

### `header.rs` — SessionHeader (7 lines → ~30 lines with docs)
- `SessionHeader` struct definition
- `impl SessionHeader`

### `context_stats.rs` — ContextStats (~25 lines)
- `ContextStats` struct
- `impl ContextStats` (estimated_remaining, utilization_percent)

### `builder.rs` — Construction (~120 lines + tests)
- `Session::new()` — persisted session
- `Session::in_memory()` — no-disk session
- `Session::with_context_manager()`
- `Session::with_token_budget()`
- `Session::with_redactor()`
- `Session::with_estimator()`
- `Session::set_model()`
- `Session::set_token_budget()`
- `Session::set_tools()`
- `Session::set_redactor()`

### `accessors.rs` — Read-only queries (~80 lines + tests)
- `Session::header()`
- `Session::leaf()`
- `Session::entry()`
- `Session::model()`
- `Session::system_prompt()`
- `Session::add_tool_result()` (convenience alias)
- `Session::get_full_result()`
- `Session::token_budget()`
- `Session::system_overhead()`
- `Session::schema_overhead()`
- `Session::message_budget()`
- `Session::redactor()`
- `Session::estimator()`
- `Session::estimator_mut()`
- `Session::entry_count()`
- `Session::save_path()`
- `Session::flush()`
- `Session::persist_state()` (pub(crate))
- `Session::set_flushed_count()` (pub(crate))

### `navigation.rs` — Tree traversal & branching (~170 lines + tests)
- `Session::path_to_root()`
- `Session::children()`
- `Session::branch_to()`
- `Session::branch_with_summary()`

### `append.rs` — All append operations (~250 lines + tests)
- `Session::append_user_message()`
- `Session::append_assistant_message()` (pub(crate))
- `Session::append_tool_result()` (with redaction + truncation)
- `Session::append_compaction()`
- `Session::append_branch_summary()`
- `Session::append_label()`
- `Session::append_custom_state()`
- `Session::append_custom_message()`
- `Session::close()`

### `truncation.rs` — Truncation helpers (~80 lines + tests)
- `MAX_TOOL_RESULT_FRACTION` constant
- `truncation_footer()`
- `floor_char_boundary()`
- `chars_to_fit_tokens()`
- `estimate_entry_tokens_for_compaction()` (session-level, distinct from compaction.rs)

### `context.rs` — Context building & LLM interaction (~120 lines + tests)
- `Session::path_messages()`
- `Session::send_current()`
- `Session::context_stats()`
- `estimate_messages_tokens()` (private helper)
- `Session::compact_older_than()` (calls into compaction.rs)

### `extensions.rs` — Typed extension entries (~120 lines + tests)
- `ExtensionEntry` trait
- `ExtensionMessageEntry` trait
- `Session::write_custom_state()`
- `Session::read_custom_state()`
- `Session::write_custom_message()`
- `Session::read_custom_message()`

### `tree.rs` — Internal tree operations (~50 lines, no public API change)
- `Session::append_entry()` (private core, called by append.rs methods)
- `Session::entries_in_order()` (pub(crate), called by persist.rs)
- `Session::new_internal()` (pub(crate), called by persist.rs)
- Helper functions: `format_message_text()`, `format_compaction_summary_text()`

### `session.rs` — Core definition (~100 lines after extraction)
- `Session` struct definition (fields only)
- `impl std::fmt::Debug for Session`
- `pub mod` declarations for all submodules
- `pub use` re-exports (keep all items accessible at `session::`)

---

## Step-by-Step TDD Execution Plan

### Pre-flight: Baseline

1. Run `cargo xtask ci` — record baseline (all tests pass, fmt clean, lint clean)
2. Run `debtmap analyze .` — record baseline debt score for comparison
3. Record test count: `cargo test -- -q 2>&1 | tail -1`

---

### Step 1: ~~Convert session.rs to session/ directory~~ — ALREADY DONE

The codebase already uses the correct new Rust module style: `session.rs` is the module root, and `session/` contains submodules (`entry.rs`, `error.rs`, `estimator.rs`, `compaction.rs`, `persist.rs`). No conversion needed.

---

### Step 2: Extract SessionHeader + ContextStats to their own files

**TDD:**
1. Write `session/header.rs` — move `SessionHeader` struct and any impl blocks
2. Write `session/context_stats.rs` — move `ContextStats` struct and impl
3. Add `pub mod header;` and `pub mod context_stats;` to `session.rs`
4. Add `pub use` re-exports in `session.rs`

**Verify:** `cargo xtask ci` — all tests pass.

**Risk:** Minimal — these are standalone structs with no Session field access.

---

### Step 3: Extract builder methods

**TDD:**
1. Write `session/builder.rs`
2. Move `Session::new()`, `Session::in_memory()`, all `with_*` builder methods, all `set_*` mutators
3. Add tests for construction edge cases if any are missing (empty system prompt, etc.)
4. Run existing construction tests (6 tests currently)

**Verify:** `cargo xtask ci`

**Risk:** Low — construction methods are self-contained.

---

### Step 4: Extract accessor methods

**TDD:**
1. Write `session/accessors.rs`
2. Move all read-only accessors: `header()`, `leaf()`, `entry()`, `model()`, `system_prompt()`, `token_budget()`, `system_overhead()`, `schema_overhead()`, `message_budget()`, `redactor()`, `estimator()`, `estimator_mut()`, `entry_count()`, `save_path()`, `get_full_result()`, `add_tool_result()`
3. Move `flush()`, `persist_state()`, `set_flushed_count()` (pub(crate))
4. Run existing accessor tests (overhead tests, budget tests, etc.)

**Verify:** `cargo xtask ci`

**Risk:** Low — accessors read fields and return values, no mutation logic.

---

### Step 5: Extract truncation helpers

**TDD:**
1. Write `session/truncation.rs`
2. Move `MAX_TOOL_RESULT_FRACTION`, `truncation_footer()`, `floor_char_boundary()`, `chars_to_fit_tokens()`, `estimate_entry_tokens_for_compaction()`
3. Write new unit tests for `floor_char_boundary` and `chars_to_fit_tokens` if not already covered (they are — 5 existing tests)
4. Move the truncation-related tests from session.rs tests module

**Verify:** `cargo xtask ci`

**Risk:** Low — pure functions, no Session access.

---

### Step 6: Extract internal tree operations

**TDD:**
1. Write `session/tree.rs`
2. Move `Session::append_entry()` (private core)
3. Move `Session::entries_in_order()` (pub(crate))
4. Move `Session::new_internal()` (pub(crate))
5. Move `format_message_text()` and `format_compaction_summary_text()` (private helpers)
6. These are called by append.rs and persist.rs, so they must be accessible via `pub(crate)` or super::

**Verify:** `cargo xtask ci`

**Risk:** Medium-low — `append_entry` is the hot path for all appends. Must maintain field access patterns.

---

### Step 7: Extract navigation methods

**TDD:**
1. Write `session/navigation.rs`
2. Move `Session::path_to_root()`, `Session::children()`, `Session::branch_to()`, `Session::branch_with_summary()`
3. Move all navigation tests (9 existing tests)
4. Add edge-case tests if any gaps (empty tree, single entry, deep trees)

**Verify:** `cargo xtask ci`

**Risk:** Low-medium — `branch_to` mutates leaf and writes audit entries, but logic is self-contained.

---

### Step 8: Extract append operations

**TDD:**
1. Write `session/append.rs`
2. Move `Session::append_user_message()`, `append_assistant_message()`, `append_tool_result()`, `append_compaction()`, `append_branch_summary()`, `append_label()`, `append_custom_state()`, `append_custom_message()`, `close()`
3. Move all append tests (20+ existing tests including truncation tests)
4. These delegate to `tree.rs::append_entry()` via `super::append_entry()` or direct field access

**Verify:** `cargo xtask ci`

**Risk:** Medium — most complex extraction due to tool-result truncation logic interacting with multiple fields (redactor, estimator, token_budget, details_store).

---

### Step 9: Extract context building

**TDD:**
1. Write `session/context.rs` (renamed from `context.rs` which already exists at `rho-core/src/context.rs` — use `session/context.rs` as the module path, types are accessed via `session::`)
2. Move `Session::path_messages()`, `Session::send_current()`, `Session::context_stats()`, `Session::compact_older_than()`
3. Move `estimate_messages_tokens()` private helper
4. Move existing tests for send_current and compact (limited — most are integration tests in agent.rs)

**Verify:** `cargo xtask ci`

**Risk:** Medium — `send_current` has the most complex async flow (build messages → stream → persist → calibrate). `compact_older_than` is async and calls the compaction strategy.

**Note:** This file sits at `session/context.rs` which is a submodule of `session/`. The existing `rho-core/src/context.rs` is the `ContextManager` trait and `TokenBudget`. These are different modules. No naming conflict at the source level; importers use `use crate::session::Session` etc.

---

### Step 10: Extract extension traits and methods

**TDD:**
1. Write `session/extensions.rs`
2. Move `ExtensionEntry` trait, `ExtensionMessageEntry` trait
3. Move `Session::write_custom_state()`, `read_custom_state()`, `write_custom_message()`, `read_custom_message()`
4. Move extension-related tests
5. Ensure re-exports in session/session.rs include `ExtensionEntry` and `ExtensionMessageEntry`

**Verify:** `cargo xtask ci`

**Risk:** Low — self-contained trait definitions and typed wrappers around `append_custom_state`/`read_custom_state`.

---

### Step 11: Final re-exports and cleanup

**TDD:**
1. Audit `session.rs` for clean re-exports
2. Ensure `lib.rs` `pub use session::{...}` still exports everything external callers need
3. Run full test suite
4. Run `cargo clippy --all-targets -- -D warnings`
5. Run `cargo fmt --all -- --check`

**Verify:** `cargo xtask ci`

---

### Step 12: Validation

1. Run `debtmap analyze .` — compare scores to baseline
2. Count lines per file — verify no file exceeds ~500 lines
3. Count methods per file — verify no file has more than ~20 methods
4. Verify all 103+ tests still pass
5. Verify no new warnings from clippy

---

## Estimated Effort

| Step | Effort | Risk |
|------|--------|------|
| 0: Baseline | 5 min | — |
| 1: Convert to directory | — | — (already done) |
| 2: Header + ContextStats | 15 min | Low |
| 3: Builder methods | 20 min | Low |
| 4: Accessor methods | 20 min | Low |
| 5: Truncation helpers | 15 min | Low |
| 6: Internal tree ops | 30 min | Medium-low |
| 7: Navigation methods | 30 min | Low-medium |
| 8: Append operations | 45 min | Medium |
| 9: Context building | 30 min | Medium |
| 10: Extensions | 15 min | Low |
| 11: Re-exports + cleanup | 20 min | Low |
| 12: Validation | 15 min | — |
| **Total** | **~4.5 hours** | |

---

## Non-Goals

- **Do NOT change the Session struct fields** — this is purely an organizational refactor
- **Do NOT change any public API** — all 56 methods remain on Session with identical signatures
- **Do NOT change any logic** — move code only, no behavior modification
- **Do NOT restructure the submodules that already exist** (entry.rs, error.rs, estimator.rs, compaction.rs, persist.rs) — they are already well-organized

## Success Criteria

1. `cargo xtask ci` passes with zero regressions
2. No file in `session/` exceeds 500 lines (excluding tests)
3. No file in `session/` has more than 20 methods on Session
4. Debtmap god object score for session drops below 30
5. All external callers compile unchanged (rho/src, rho-tools/src, etc.)
