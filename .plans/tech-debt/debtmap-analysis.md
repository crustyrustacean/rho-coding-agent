# Debtmap Analysis Report

**Date:** 2026-05-20  
**Tool:** debtmap v0.16.5  
**Project:** rho-coding-agent  
**Version:** 0.36.9 (pre-Phase 3.10)

---

## Executive Summary

**Overall Health:** Good - Well-tested core with moderate technical debt

- **Total Debt Score:** 1,525
- **Debt Density:** 84.33 per 1K LOC (acceptable for AI agent codebase)
- **Total LOC:** 18,086
- **Severity Breakdown:**
  - Critical: 2 items
  - High: 7 items
  - Medium: 17 items
  - Low: 8 items

**Key Finding:** Most "debt" items are well-tested core infrastructure rather than actual maintainability issues. The architecture follows good separation of concerns, and test coverage is strong.

---

## Top 10 Items by Score

| Rank | Location | Score | Severity | Type | Description |
|------|----------|-------|----------|------|-------------|
| 1 | `rho-core/src/agent/mod.rs:210` | 211 | Critical | High Complexity | `run_loop()` - Agent kernel orchestration |
| 2 | `rho-core/src/session.rs` | 133 | Critical | God Object | 55 methods, 12 fields, 9 responsibilities |
| 3 | `rho-tools/src/files.rs:405` | 93 | High | High Complexity | `EditFile::execute()` - File edit validation |
| 4 | `rho-tools/src/files.rs:237` | 83 | High | High Complexity | `EditFile::execute()` - File edit validation |
| 5 | `rho-core/src/client/mod.rs:404` | 60 | High | High Complexity | `try_next_chunk()` - SSE chunk parsing |
| 6 | `rho-core/src/context.rs:410` | 60 | High | High Complexity | `fit()` - Context fitting algorithm |
| 7 | `rho-bench/src/harness.rs:100` | 53 | High | High Complexity | `run_benchmarks()` - Benchmark orchestration |
| 8 | `rho-tools/src/rust/rustdoc.rs:610` | 52 | High | High Complexity | `decode_html_entities()` - HTML entity decoding |
| 9 | `rho-tools/src/rust/format.rs:12` | 51 | High | High Complexity | `format_diagnostics_for_model()` - Diagnostic formatting |
| 10 | `rho-core/src/sandbox.rs:303` | 49 | Medium | High Complexity | `canonicalize_for_write()` | Path normalization |

---

## Critical Items

### 1. Agent Loop (`run_loop`) - Score: 211

**Location:** `rho-core/src/agent/mod.rs:210`

**Metrics:**
- Cyclomatic Complexity: 23 (dampened: 13)
- Cognitive Complexity: 66
- Nesting Depth: 5
- Lines: 248

**Analysis:**
- **Not actual debt** - This is the central orchestrator function, expected to be complex
- 33 upstream callers (3 production, 30 test)
- 12 downstream callees
- High test coverage indicates stable, well-tested core
- **Architectural Insight:** Well-Tested Core - not actual debt

**Action:** None. Complexity is inherent to agent loop orchestration.

---

### 2. Session Module - Score: 133

**Location:** `rho-core/src/session.rs`

**Metrics:**
- Lines: 3,880
- Functions: 170
- Average Complexity: 1.5
- Methods: 55, Fields: 12, Responsibilities: 9
- Coupling: Ca=4 (highly coupled), Ce=56

**Analysis:**
- **God Object** - Large file with many responsibilities
- 56 downstream callees (critical path)
- Instability: 0.93 (high - I=Ce/(Ca+Ce) = 4/60 ≈ 0.93)
- **Impact:** -52 complexity, -104.7 maintainability improvement if split

**Recommended Action:** URGENT

Split by data flow into focused modules:
1. **Input/parsing** - Session construction and deserialization
2. **Core logic/transformation** - Compaction, branching, tree operations
3. **Output/formatting** - Serialization, display helpers

Target: 6 focused modules with <30 functions each.

**Estimated Effort:** 20-30 hours

---

## High Priority Items

### 3-4. EditFile execute() Functions - Scores: 93, 83

**Location:** `rho-tools/src/files.rs:237` and `:405`

**Metrics:**
- Cyclomatic: 19, 13
- Cognitive: 27, 22
- Nesting: 2, 3
- Lines: 90, 149

**Analysis:**
- **Pre-Phase 3.10 Complexity** - Already flagged for hashline enhancement
- Both are `EditFile::execute()` (different tool variants or same function)
- Complex validation logic for exact-match replacement
- Multiple error paths, nested conditionals

**Action:** Defer to Phase 3.10

The hashline implementation will restructure this code:
- Extract hash computation to `hashline.rs`
- Simplify validation by using hash anchors instead of exact text matching
- Reduce cyclomatic complexity through cleaner logic

**Phase 3.10 Relevance:** High - This is exactly what hashline editing will refactor

---

### 5-6. Client/Context Functions - Scores: 60, 60

**Location:** `rho-core/src/client/mod.rs:404`, `rho-core/src/context.rs:410`

**Metrics:**
- `try_next_chunk()`: Cyclomatic 14, Cognitive 25
- `fit()`: Cyclomatic 14, Cognitive 26

**Analysis:**
- Streaming chunk parsing and context fitting are inherently complex
- Both handle edge cases, error recovery, state management
- **Well-tested** - Core infrastructure with high test coverage
- **Architectural Insight:** Stable foundation

**Action:** None. Complexity is acceptable for these functions.

---

### 7. Benchmark Harness - Score: 53

**Location:** `rho-bench/src/harness.rs:100`

**Metrics:**
- Cyclomatic: 7
- Cognitive: 19 → 11 (entropy-adjusted)
- Nesting: 4
- Lines: 88

**Analysis:**
- Dev-only benchmark harness
- **No production callers** - test-only code
- Complexity is for flexibility in benchmark scenarios

**Action:** None. Dev-only code with no production impact.

---

### 8. HTML Entity Decoder - Score: 52

**Location:** `rho-tools/src/rust/rustdoc.rs:610`

**Metrics:**
- Cyclomatic: 24
- Cognitive: 39
- Nesting: 3
- Lines: 64

**Analysis:**
- HTML entity decoding is inherently complex (many HTML entities)
- 7 upstream callers (1 production, 6 test)
- **Well-tested** - High test coverage
- **Architectural Insight:** Well-Tested Core

**Action:** Consider extracting entity lookup table for maintainability, but low priority.

---

### 9-10. Diagnostic & Path Functions - Scores: 51, 49

**Location:** `rho-tools/src/rust/format.rs:12`, `rho-core/src/sandbox.rs:303`

**Analysis:**
- Diagnostic formatting and path normalization are complex by nature
- Handle many edge cases and error conditions
- Both are well-tested and stable

**Action:** None. Acceptable complexity for these domains.

---

## God Object Analysis

### Files Identified

| File | Score | Lines | Methods | Responsibilities | Action |
|------|-------|-------|---------|----------------|--------|
| `rho-tools/src/shell.rs` | 20 | 618 | - | 9 domains | Low priority |
| `rho-tools/src/rust/rustdoc.rs` | 8 | 832 | 12 | 6 domains | Monitor |
| `rho-core/src/session.rs` | 44 | 3,880 | 55 | 9 domains | **URGENT** |
| `rho-core/src/session/entry.rs` | 2 | 217 | 22 | 0 | Low priority |
| `rho-highlight/src/query.rs` | 3 | 239 | 25 | 3 | Low priority |
| Test files (x3) | Various | 1284-1605 | 53-69 | 7-9 | Test files - N/A |

### Session.rs Split Recommendation

```
Current: session.rs (3,880 lines, 170 functions, 9 responsibilities)

Proposed modules:
├── session.rs (core)           # 300 lines, 15 functions
├── session/mod.rs             # Main struct, entry point
├── session/entry.rs           # Entry types and constructors
├── session/persist.rs         # JSONL persistence
├── session/compaction.rs       # Compaction strategies
├── session/tree.rs            # Tree operations and navigation
└── session/validation.rs       # Validation helpers
```

**Benefits:**
- -104.7 maintainability improvement
- Smaller, focused modules
- Easier to test and reason about
- Reduces coupling (currently Ca=4, Ce=56)

**Phase:** Post-Phase 3.10 (Phase 3.11 or Phase 4 pre-work)

---

## Test File Considerations

Three test files flagged as "God Objects":
- `rho-core/tests/integration_tests.rs` (1,605 lines, 53 functions)
- `rho-tools/tests/tool_tests.rs` (1,284 lines, 69 functions)
- `rho-eval/src/tasks.rs` (multiple verify functions)

**Architectural Insight:** These are **not actual debt** - they are:
- Well-Tested Core (high test coverage, stable)
- Test helpers and fixtures
- Organized by domain/purpose within test scope

**Action:** None. Test file organization is acceptable.

---

## Complexity Distribution

### By Crate

| Crate | Total Score | Critical | High | Medium | Low | Primary Debt Types |
|-------|-------------|----------|------|--------|-----|-------------------|
| rho-core | ~1,100 | 1 | 1 | 1 | 0 | God Object (session.rs) |
| rho-tools | ~350 | 0 | 3 | 2 | 0 | High Complexity |
| rho-bench | ~100 | 0 | 2 | 0 | 0 | High Complexity |
| rho-eval | ~50 | 0 | 0 | 1 | 0 | High Complexity |
| rho-highlight | ~25 | 0 | 0 | 1 | 0 | High Complexity |

### By Type

| Type | Count | Total Score | Notes |
|------|-------|-------------|-------|
| God Object | 6 | ~100 | Test files and session.rs |
| High Complexity | 28 | ~1,425 | Majority of debt score |

---

## Relevance to Phase 3.10 (Hashline Editing)

### Direct Impact

**Positive:**
- Phase 3.10 will **reduce** complexity in `rho-tools/src/files.rs`
- Current EditFile execute() functions (scores 93, 83) will be refactored
- Hashline extraction to new module `hashline.rs` will improve organization

**Neutral:**
- Debtmap correctly identifies EditFile as complex before hashline implementation
- The refactor will validate hashline's architectural benefits

### Session.rs Impact

**Critical dependency:** Hashline editing depends on `rho-core::session::Session` for JSONL persistence.

**Recommendation:**
- Defer session.rs refactoring to **Phase 3.11 or Phase 4 pre-work**
- Complete Phase 3.10 first to validate hashline approach
- Use session.rs refactoring as opportunity to test improved architecture

**Rationale:** Session.rs refactoring is larger and riskier than hashline. Hashline first provides:
1. Immediate value (reliable edits)
2. Reference implementation for other refactors
3. Validation of architectural improvements
4. Foundation for TUI integration

---

## Recommendations Summary

### Immediate Actions (Phase 3.10)

1. **Proceed with hashline editing** as planned
   - Will reduce complexity in `rho-tools/src/files.rs`
   - Extract hash computation to new module
   - Improve EditFile maintainability

2. **Document findings** in Phase 3.10 readiness assessment
   - Update debtmap analysis section
   - Note that some items will be resolved by Phase 3.10

### Near-Term Actions (Phase 3.11)

1. **Refactor session.rs** (URGENT)
   - Split into 6 focused modules
   - Reduce responsibilities from 9 to ~2 per module
   - Estimated: 20-30 hours
   - Priority: High

2. **Review rustdoc.rs** (MEDIUM)
   - Consider extracting entity lookup table
   - Monitor for further complexity growth
   - Priority: Low

### Deferred Actions

1. **Test file reorganization** - LOW PRIORITY
   - Current organization is acceptable
   - Well-tested, not actual debt

2. **Client/context functions** - MONITOR
   - Acceptable complexity for streaming/context fitting
   - No action unless complexity grows

3. **Dev-only code** - IGNORE
   - rho-bench functions have no production impact
   - Complexity is for flexibility

---

## Comparison to Industry Standards

| Metric | rho-coding-agent | Typical Range | Status |
|--------|------------------|---------------|--------|
| Debt Density | 84.33/1K LOC | 50-150/1K | ✅ Acceptable |
| God Objects | 1 prod + 5 tests | 2-5 prod | ✅ Good |
| Critical Items | 2 | 0-3 | ⚠️ Acceptable |
| High Items | 7 | 5-10 | ✅ Acceptable |
| Total LOC | 18,086 | 10K-50K | ✅ Appropriate size |
| Test Coverage | High (inferred) | Good | ✅ Good practice |

**Overall Assessment:** The codebase is well-architected and tested. Technical debt is manageable and mostly consists of inherent complexity for an AI agent's core functions (agent loop, session management, streaming). The one urgent item (session.rs) can be addressed in a focused phase after hashline editing.

---

## Files

- `debtmap_analysis.txt` - Raw terminal output (109 lines)
- `debtmap_analysis.md` - Full markdown report (1,915 lines)

**Original Report Location:** `.plans/` directory (this file)

---

**Generated:** 2026-05-20T13:40:13Z  
**Tool Version:** debtmap 0.16.5