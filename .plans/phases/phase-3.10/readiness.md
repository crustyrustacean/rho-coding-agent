# Phase 3.10 Readiness Assessment

**Date:** 2026-05-20 | **Phase:** 3.10 - Hashline Editing | **Status:** 🔜 Ready to Start

---

## Executive Summary

**Ready to start Phase 3.10 development.**

All prerequisites are in place. The architecture cleanly separates file operations in `rho-tools`, making hashline editing a self-contained enhancement. Reference implementations have been studied and evaluated. The implementation strategy is clear: single-phase combined implementation with backward compatibility.

---

## ✅ What's Ready

### Architecture — Strong

`rho-tools/src/files.rs` is a focused module (~850 lines) containing `ReadFile`, `WriteFile`, `ListDir`, and `EditFile`. Hashline editing is a localized enhancement to two tools (`ReadFile`, `EditFile`) without requiring changes to `rho-core` or other crates.

### Debtmap Analysis Complete

A technical debt analysis was completed using debtmap v0.16.5 before starting Phase 3.10:

**Overall Assessment:** Good - Well-tested core with moderate technical debt
- **Total Debt Score:** 1,525
- **Debt Density:** 84.33 per 1K LOC (acceptable for AI agent codebase)
- **Total LOC:** 18,086

**Key Findings for Phase 3.10:**

1. **EditFile execute() flagged as complex (scores: 93, 83)**
   - Current implementation has high cyclomatic complexity (19, 13)
   - Multiple nested conditionals and validation paths
   - **Phase 3.10 will address this:** Hashline refactoring extracts complexity to `hashline.rs`

2. **No dependencies on problematic areas**
   - Session.rs (score: 133, God Object) is a dependency but hashline uses it correctly
   - Shell.rs (score: 20, God Object) not involved in hashline editing

3. **Well-tested core**
   - Most high-complexity items are well-tested (indicated by "Well-Tested Core" classification)
   - This indicates stable foundation for refactoring

4. **Session.rs refactoring recommended for Phase 3.11 or Phase 4 pre-work**
   - URGENT: 3,880 lines, 55 methods, 9 responsibilities
   - Estimated effort: 20-30 hours
   - Split into 6 focused modules

**Recommendation from debtmap analysis:**
> Proceed with hashline editing as planned. The refactor will:
> 1. Reduce complexity in `rho-tools/src/files.rs`
> 2. Extract hash computation to new module
> 3. Improve EditFile maintainability

Full analysis saved as `.plans/debtmap-analysis.md`.

### Tool Infrastructure — Mature

- `Tool` trait and `ToolRegistry` are battle-tested across 560+ tests
- `ToolResult` supports rich details via `ToolResultDetails` enum
- JSON schema generation for tool parameters is well-established
- Tool descriptions and prompts are cleanly separated

### Error Handling — Established

`ToolError` enum provides structured error types (`MissingArgument`, `ValidationFailed`, etc.). Hashline validation errors fit naturally into this framework. Error messages flow through the agent loop and are surfaced to the model for recovery.

### Testing Infrastructure — Solid

- Unit tests in `#[cfg(test)]` blocks within each source file
- Integration tests in `rho-tools/tests/`
- `rho-test-helpers` provides `FileTestEnv` for filesystem testing
- `rho-eval` provides behavioral validation with 5 canonical tasks
- Test suite audit completed in Phase 3 (708 tests passing)

### Reference Implementations — Studied

Multiple production implementations analyzed:
- `pi-hashline-edit`: TypeScript, most relevant for API design
- `oh-my-pi`: Production-hardened, proven at scale
- `hashline-tools`: Rust CLI, relevant for Rust integration
- Design decisions documented in knowledge base

### Decision Analysis — Complete

- Line numbering vs hashline: Documented decision (combined implementation)
- Format choice: `LINE#HASH:` with custom alphabet
- Backward compatibility: Dual-format support strategy defined
- TUI compatibility: Analyzed and confirmed (see knowledge base)

---

## ⚠️ Considerations

### Hash Algorithm Choice — Resolved

**Concern:** Should we use xxHash32 (standard) or custom implementation?

**Decision:** Custom implementation.

**Rationale:**
- xxHash32 adds ~150KB to binary size for minimal gain
- 2-character hash is collision-resistant enough for this use case
- Reference implementations use custom approaches
- Easier to test, port, and reason about
- Performance difference is negligible for typical file sizes

### Model Adaptation — Managed

**Concern:** Changing ReadFile output format requires model adaptation.

**Mitigation:**
- Format change is well-documented and descriptive
- System prompt will explain hashline usage clearly
- Backward compatibility via `hashline: false` parameter
- Gradual rollout: Can monitor performance and adjust prompts

### Format Complexity — Acceptable

**Concern:** Hashline format adds visual noise to file content.

**Mitigation:**
- Hashes are short (2 chars) and dimmed in TUI (Phase 4)
- Model benefits outweigh minor visual cost
- Format is consistent and learnable
- Toggle visibility in TUI provides clean view when needed

### Edge Cases — Addressed in Tasks

Identified and planned for:
- Empty files (advisory message)
- Binary files (rejected with error)
- Very long lines (no truncation, handled as-is)
- Non-alphanumeric lines (line-number-based hashing)
- Large files (dynamic padding, performance validated)

---

## 🔴 Pre-Work Items

None. All prerequisites are complete.

---

## Task Path

Recommended execution order (from tasks.md):

| Order | Task | Rationale |
|---|---|---|
| 1 | Hash computation module | Foundation — everything depends on this |
| 2 | Extend ReadFile | Provides hashline output for use |
| 3 | Extend EditFile | Consumes hashline output, enables edits |
| 4 | Hash mismatch error recovery | Critical safety feature |
| 5 | Diff generation | Enhances edit results, enables chained edits |
| 6 | Chained edit support | Improves efficiency for multi-edit workflows |
| 7 | Update prompts | Model needs to know how to use new format |
| 8 | Backward compatibility tests | Ensures no regressions |
| 9 | Error handling tests | Validates all failure modes |
| 10 | Integration tests | End-to-end validation |
| 11 | Documentation updates | Completes feature delivery |
| 12 | Performance validation | Ensures acceptable overhead |
| 13 | TUI preparation | Documents format for Phase 4 |
| 14 | Feature flag evaluation | Final configuration decision |

This ordering ensures each task builds on the previous one, with foundational work (hash computation) landing first, core functionality (ReadFile + EditFile) in the middle, and polish (documentation, testing, TUI prep) at the end.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Hash collisions causing false negatives | Low | Medium | 2-char hash over 16-letter alphabet gives 256 combos. Line number provides disambiguation. Extremely unlikely in practice. |
| Model struggles with new format | Medium | Medium | Clear system prompt, examples in AGENTS.md, backward compatibility fallback. Monitor eval metrics post-deployment. |
| Performance degradation on large files | Low | Low | Hash computation is O(n) with small constant. Benchmark 10k-line files to validate. |
| Regression in legacy format | Low | High | Comprehensive backward compatibility tests. All existing eval scenarios must pass. |
| TUI integration complexity (Phase 4) | Low | Low | Format is parseable with simple regex. Document TUI requirements now, implement later. |

---

## Success Metrics

- All existing tests pass (no regressions)
- New tests cover >90% of hashline code paths
- eval pass rate does not decrease (may increase due to fewer stale-context failures)
- Token usage for typical edits decreases by ~20% (model uses anchors instead of quoting)
- Hash computation <1ms for 1000-line files
- TUI can parse format (validated in Phase 4)

---

## Dependencies

**New crate dependency:** None (custom hash implementation)

**Updated crates:**
- `rho-tools` (hashline module, ReadFile changes, EditFile changes)

**No changes to:**
- `rho-core` (hashline is tool-level concern)
- `rho-highlight` (unchanged, but TUI may use it later)
- `rho-eval` (may add new test scenario)
- `rho-bench` (no changes expected)

---

## Estimated Effort

| Task | Estimated Effort |
|------|------------------|
| Hash computation module | 2-3 hours |
| ReadFile extension | 3-4 hours |
| EditFile extension | 6-8 hours |
| Error recovery | 2-3 hours |
| Diff generation | 3-4 hours |
| Chained edits | 2-3 hours |
| Prompt updates | 1-2 hours |
| Testing | 4-6 hours |
| Documentation | 2-3 hours |
| **Total** | **25-36 hours** |

**Velocity assumption:** 2-3 days of focused development for a single developer, or 1-2 days if working in collaboration.

---

## Blockers

None identified.

---

## Next Steps

1. Create `rho-tools/src/hashline.rs` with hash computation module
2. Begin Task 1: Add hash computation unit tests
3. Proceed through tasks in order
4. Update CHANGELOG.md upon completion
5. Tag release as v0.37.0 (or next available version)

---

**Last updated:** 2026-05-20