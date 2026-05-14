# Refactor: Clean up `rho-core/src/client.rs`

**Status:** 📋 Planned | **Branch:** `refactor/client-cleanup` | **Created:** 2026-05-14

---

## Overview

Clean up code smells in `rho-core/src/client.rs` module:
- Remove duplicate default endpoint value
- Clarify constructor documentation
- Remove duplicate test comments
- Organize tests logically

**Estimated time:** ~45 minutes
**Risk level:** Low (self-contained refactor with good test coverage)
**Breaking changes:** None (API unchanged)

---

## Motivation

The `rho-core/src/client.rs` module has several code smells:

1. **Duplicate default endpoint** — Same URL appears in two places:
   - `DEFAULT_ENDPOINT` constant used only in `client_factory()`
   - Hardcoded in `LocalChatClient::new()` constructor

2. **Potentially redundant constructor** — `LocalChatClient::new()` may not be needed since:
   - `client_factory()` is the production path (used by both `rho` and `rho-bench`)
   - `with_endpoint()` is more flexible
   - Tests can use `with_endpoint()` or `client_factory()`

3. **Duplicate test comment** — Two identical `// ── Endpoint derivation ──────────────────────────────────────────────` comment blocks in tests

---

## Tasks

| # | Task | Status |
|---|-------|--------|
| 1 | Remove duplicate default endpoint | ⏸️ Not started |
| 2 | Evaluate whether to keep `new()` constructor | ⏸️ Not started |
| 3 | Remove duplicate test comment | ⏸️ Not started |
| 4 | Update documentation for constructors | ⏸️ Not started |
| 5 | Verify all tests still pass | ⏸️ Not started |
| 6 | Update `AGENTS.md` (if needed) | ⏸️ Not started |

---

## Files

| File | Purpose |
|------|----------|
| `.plans/refactor-client-cleanup/README.md` | Quick reference and overview |
| `.plans/refactor-client-cleanup/plan.md` | Detailed plan with motivation & solution steps |
| `.plans/refactor-client-cleanup/tasks.md` | Detailed checklist for each task |

---

## Success Criteria

- [ ] Default endpoint defined in only one place
- [ ] All constructors have clear documentation explaining when to use them
- [ ] Test module well-organized with clear section headers
- [ ] All tests pass (`cargo test --package rho-core`)
- [ ] Full CI passes (`cargo xtask ci`)
- [ ] No new clippy warnings introduced
- [ ] Documentation builds successfully (`cargo doc --open`)

---

## Related Work

- **Phase 3.4.0** — Added `client_factory()` and shared bootstrapping to `rho-core`
- **AGENTS.md** — May need updating to reflect recommended usage patterns

---

## Quick Start

```bash
# Checkout branch
git checkout refactor/client-cleanup

# Start working
cd rho-core/src
edit client.rs

# Run tests
cargo test --package rho-core

# Run full CI when done
cargo xtask ci
```

---

## Pull Request

When ready, create a PR from:
- `branch: refactor/client-cleanup`
- `to: trunk`

PR template should include:
- Summary of changes
- Verification steps (CI passes)
- Related issues or phases

---

**Last updated:** 2026-05-14
