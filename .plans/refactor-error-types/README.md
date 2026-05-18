# Refactor: Domain-Specific Error Types

**Status:** 📋 Planned  
**Branch:** `refactor/error-types`  
**Created:** 2026-05-17

Quick reference for replacing the centralized `RhoError` with per-domain error types.

---

## Overview

Replace `rho_core::error::RhoError` (a single enum with 8 variants + `Unexpected(anyhow)` catch-all) with domain-specific error enums colocated with their origins. The `Tool::execute` return type (`Result<ToolOutcome>`) already decouples decouples tool errors from core errors — this migration completes that decoupling by giving each crate its own error types.

Currently `rho-tools` shoves all errors into `RhoError::Unexpected(anyhow!())` for every error — stripping all structure. This plan carves `RhoError` into focused enums and gives `rho-tools` a proper `ToolError`.

**Estimated time:** ~4–6 hours  
**Risk level:** Medium (touches every crate; good test coverage needed)

---

## Quick Start

```bash
cd rho-coding-agent
git checkout -b refactor/error-types

# Work through tasks in order (see tasks.md)
# Phase 1: rho-tools gets its own error type
# Phase 2: rho-core error modules
# Phase 3: Clean up rho-highlight
# Phase 4: Remove old RhoError, thin boundary enum
# Phase 5: Wire the binary and CI

cargo xtask ci  # Must pass at the end
```

---

## Files

| File | Purpose |
|------|---------|
| `plan.md` | Detailed plan with motivation, architecture, and phases |
| `tasks.md` | Granular checklist for each task |
| `README.md` | This file — quick reference |

---

## Phases Overview

| # | Phase | What changes | Est. time |
|---|-------|-------------|-----------|
| 1 | `rho-tools` error type | New `rho-tools/src/error.rs`, migrate `shell.rs`/`files.rs` from `RhoError::Unexpected` | ~60 min |
| 2 | `rho-core` per-module errors | Carve `client.rs`, `agent.rs`, `session.rs`, `sandbox.rs`, `config.rs` errors out of `RhoError` | ~120 min |
| 3 | `rho-highlight` audit | Ensure `HighlightError` is consistent with new conventions | ~30 min |
| 4 | Remove old `RhoError` | Thin boundary enum or trait for retry/abort decisions. Update re-exports in `lib.rs` | ~60 min |
| 5 | Wire the binary | Fix any remaining references in `rho-bench`, `rho-test-helpers`, `rho` binary | ~30 min |
| 6 | CI & docs | Full `cargo xtask ci`, update `AGENTS.md`, verify docs | ~30 min |

---

## Success Criteria

- [ ] `rho-tools` has a proper `ToolError` enum (no more `RhoError::Unexpected` in `rho-tools`)
- [ ] `rho-core` errors are colocated: `client::error`, `agent::error`, `session::error`, `sandbox::error`, `config::error`
- [ ] `RhoError` is removed (or reduced to a thin boundary enum with `From` impls)
- [ ] Retry/abort logic still works via a shared trait or thin enum
- [ ] All tests pass (`cargo xtask ci`)
- [ ] No new clippy warnings
- [ ] Documentation builds cleanly
- [ ] `AGENTS.md` is updated with the new error architecture

---

## Related Documents

- `../../AGENTS.md` — Project conventions
- `../../ARCHITECTURE.md` — Current architecture (will need updating)
- `../../rho-core/src/error.rs` — Target for removal
- `../../rho-tools/src/shell.rs` — Heaviest user of the catch-all
