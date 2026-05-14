# Refactor: Clean up `rho-core/src/client.rs`

Quick reference for the client.rs cleanup refactor.

---

## Overview

Clean up code smells in `rho-core/src/client.rs`:
- Remove duplicate default endpoint value
- Clarify constructor documentation
- Remove duplicate test comments
- Organize tests logically

**Estimated time:** ~45 minutes
**Risk level:** Low (self-contained refactor with good test coverage)

---

## Quick Start

```bash
# Start working (already on refactor/client-cleanup branch)
cd rho-core/src
edit client.rs

# Run tests
cargo test --package rho-core

# Run full CI when done
cargo xtask ci
```

---

## Files

| File | Purpose |
|------|----------|
| `plan.md` | Detailed plan with motivation and solution steps |
| `tasks.md` | Detailed checklist for each task |
| `README.md` | This file — quick reference |

---

## Tasks Overview

| # | Task | Status |
|---|-------|--------|
| 1 | Remove duplicate default endpoint | ⏸️ Not started |
| 2 | Evaluate whether to keep `new()` constructor | ⏸️ Not started |
| 3 | Remove duplicate test comment | ⏸️ Not started |
| 4 | Update documentation for constructors | ⏸️ Not started |
| 5 | Verify all tests still pass | ⏸️ Not started |
| 6 | Update `AGENTS.md` (if needed) | ⏸️ Not started |

---

## Success Criteria

- [ ] Default endpoint defined in only one place
- [ ] All constructors have clear documentation
- [ ] Test module well-organized
- [ ] All tests pass (`cargo test --package rho-core`)
- [ ] Full CI passes (`cargo xtask ci`)
- [ ] No new clippy warnings

---

## Related Documents

- `../phases/phase-3/phase.md` — Phase 3 implementation
- `../../AGENTS.md` — Main project documentation
- `rho-core/src/client.rs` — Target file for refactor

---

## Commands

```bash
# View detailed plan
cat .plans/refactor-client-cleanup/plan.md

# View task checklist
cat .plans/refactor-client-cleanup/tasks.md

# Check test status
cargo test --package rho-core 2>&1 | grep "test result"

# Run clippy
cargo clippy --package rho-core -- -D warnings

# Build docs
cargo doc --package rho-core --open
```
