# Phase 3.10: Hashline Editing

## Overview

**Goal:** Enable reliable, content-addressed file edits using hash-anchored line references.

**Status:** 🔜 In Planning

**Estimated Effort:** 25-36 hours (2-3 days focused development)

## Motivation

Address the "harness problem" where AI agents make incorrect edits due to stale context. When a file changes between read and write operations:

- Line numbers become unreliable
- Exact text matches fail
- Models waste tokens quoting full lines
- Corrupted edits require manual recovery

Hashline editing provides:
- **Prevention:** Edits fail safely instead of corrupting the wrong line
- **Recovery:** Error messages include fresh hashes for immediate retry
- **Efficiency:** Models use short anchors (`9#KT`) instead of quoting full lines
- **Verification:** Hash mismatches detect out-of-sync state before writing

## Key Files

- **Implementation plan:** `phase.md`
- **Detailed tasks:** `tasks.md`
- **Readiness assessment:** `readiness.md`
- **Research references:** Knowledge base documents:
  - `Hashline Editing Research for rho-coding-agent` (ID: `0c0e85da-eedf-40f0-9cef-863cd184a756`)
  - `Line Numbering vs Hashline: Implementation Strategy Decision` (ID: `a9919194-9f87-4d58-85d1-223b2ed23c20`)
  - `Hashline Editing TUI Compatibility Analysis` (ID: `322abe5f-8fc9-4de1-9a0a-3486d74d8070`)

## Format Design

**ReadFile output (hashline enabled):**
```text
<context>
 8#VR:function hello() {
 9#KT:  console.log("world");
10#BH:}
<context:end>
```

**EditFile hashline format:**
```json
{
  "path": "src/main.ts",
  "edits": [
    { "op": "replace", "pos": "9#KT", "lines": ["  console.log('hashline');"] }
  ]
}
```

**Supported operations:** `replace`, `append`, `prepend`, `delete`

**Hash format:** 2 characters from alphabet `ZPMQVRWSNKTXJBYH` (excludes hex, vowels, ambiguous letters)

## Architecture

### Module Layout

```
rho-tools/src/
├── files.rs           # ReadFile, EditFile (modified)
└── hashline.rs        # New: Hash computation module (created)
```

### Dependencies

**New:** None (custom hash implementation to minimize dependencies)

**Modified:** `rho-tools` only

**No changes to:** `rho-core`, `rho-highlight`, other crates

## Tasks Overview

14 main tasks in logical order:

1. Hash computation module (foundation)
2. Extend ReadFile to output hashline format
3. Extend EditFile to support hashline anchors
4. Hash mismatch error recovery
5. Diff generation for edit results
6. Chained edit support with updated anchors
7. Update system prompts and tool descriptions
8. Backward compatibility tests
9. Comprehensive error handling tests
10. End-to-end integration tests
11. Documentation updates
12. Performance validation
13. TUI preparation (documentation)
14. Feature flag evaluation

See `tasks.md` for detailed breakdown.

## Testing Strategy

- **Unit tests:** Hash computation, anchor parsing, edit operations, diff generation
- **Integration tests:** ReadFile/EditFile with hashline, error recovery
- **End-to-end tests:** New eval scenario for stale-context recovery
- **Regression tests:** All existing tests must pass, no format breakage
- **Performance tests:** Validate hash computation overhead

## Success Criteria

1. ✅ ReadFile outputs hashline format by default
2. ✅ EditFile accepts and validates hash anchors
3. ✅ Hash mismatches fail with helpful errors + fresh hashes
4. ✅ Legacy `old_text`/`new_text` edits continue to work
5. ✅ All existing tests pass (no regressions)
6. ✅ New tests cover hashline functionality
7. ✅ System prompts updated
8. ✅ Documentation complete
9. ✅ eval scenarios validate improved reliability
10. ✅ TUI can parse format (documented for Phase 4)

## Key Decisions

### Combined Implementation

Line numbers are implemented **together** with hashes, not as a separate phase. Rationale:
- Line numbers have limited standalone value
- Double format change causes model churn
- Reference implementations did it this way
- Combined approach is ~10% less total work

### Backward Compatibility

EditFile accepts both formats transparently:
- Hashline: `{op: "replace", pos: "9#KT", lines: [...]}`
- Legacy: `{old_text: "...", new_text: "..."}`

Detection based on presence of `"op"` field.

### Opt-Out Mechanism

ReadFile accepts `hashline: false` parameter to return plain `<context>` format for users who prefer legacy behavior.

### Custom Hash Algorithm

No xxHash32 dependency. Custom implementation:
- Based on line content for lines with alphanumerics
- Based on line number for lines without alphanumerics
- 2-character hash from 16-letter alphabet
- ~50 lines of code, 0KB binary size increase

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Hash collisions | Low | Medium | 2-char hash = 256 combos, line number disambiguation |
| Model adaptation | Medium | Medium | Clear prompts, examples, backward compatibility |
| Performance | Low | Low | O(n) with small constant, benchmark 10k-line files |
| Legacy regression | Low | High | Comprehensive tests, all eval scenarios pass |
| TUI complexity | Low | Low | Parseable with regex, documented for Phase 4 |

## Next Steps

1. Review `readiness.md` for detailed assessment
2. Review `tasks.md` for implementation plan
3. Create `rho-tools/src/hashline.rs`
4. Begin Task 1: Hash computation module
5. Proceed through tasks sequentially

## References

External implementations studied:
- [pi-hashline-edit](https://github.com/RimuruW/pi-hashline-edit)
- [pi-hashline-readmap](https://github.com/coctostan/pi-hashline-readmap)
- [oh-my-pi](https://github.com/can1357/oh-my-pi)
- [hashline-tools](https://github.com/gtrak/hashline-tools)
- [The Harness Problem](https://blog.can.ac/2026/02/12/the-harness-problem/)

Knowledge base documents (search by tag: `hashline`, `rho-coding-agent`):
- Hashline editing research and technical details
- Implementation strategy decision analysis
- TUI compatibility assessment