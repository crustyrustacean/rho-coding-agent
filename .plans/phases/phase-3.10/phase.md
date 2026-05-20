# Phase 3.10: Hashline Editing

**Goal:** Enable reliable, content-addressed file edits using hash-anchored line references. Edits reference content hashes (`LINE#HASH:`) instead of raw text, preventing stale-context corruption.

**Milestone:** The agent reads files with hashlined output, edits using hash anchors, and receives helpful error messages with fresh anchors when files have changed. Backward compatibility with legacy `old_text`/`new_text` edits is maintained.

**Current state (pre-Phase 3.10):** `ReadFile` returns plain content wrapped in `<context>` tags. `EditFile` uses exact-match replacement with `old_text`/`new_text`, ensuring uniqueness and no overlap. No line numbers or content hashes anywhere in the file editing workflow. The model must quote full lines to make edits, leading to token waste and stale-context failures when files change between read and edit operations.

**Debtmap analysis:** A technical debt analysis was completed using debtmap v0.16.5 before planning this phase:
- **Overall:** Good - Well-tested core with moderate technical debt (Debt Density: 84.33/1K LOC, Total: 18,086 LOC)
- **Key Finding:** EditFile execute() functions are flagged as high complexity (scores: 93, 83)
- **Relevance:** Phase 3.10 will **reduce** this complexity by extracting hash computation to new `hashline.rs` module
- **Session.rs** identified as God Object (score: 133, URGENT) - recommended for Phase 3.11 or Phase 4 pre-work
- Full analysis saved in `.plans/debtmap-analysis.md`
- **Recommendation:** Proceed with Phase 3.10; hashline refactoring will improve EditFile maintainability and validate architectural improvements before session.rs refactor

**Motivation:** Address the "harness problem" where AI agents make incorrect edits due to stale context. When a file changes between read and write operations, line numbers become unreliable and exact text matches fail. Hashline editing provides:
- **Prevention:** Edits fail safely instead of corrupting the wrong line
- **Recovery:** Error messages include fresh hashes for immediate retry
- **Efficiency:** Models use short anchors instead of quoting full lines
- **Verification:** Hash mismatches detect out-of-sync state before writing

**Reference implementations studied:**
- `pi-hashline-edit` (RimuruW) — Most relevant, TypeScript-based pi-coding-agent extension
- `pi-hashline-readmap` (coctostan) — Advanced with structural maps and symbol navigation
- `oh-my-pi` (can1357) — Original hashline concept, proven in production
- `hashline-tools` (gtrak) — Rust CLI implementation relevant for Rust integration

## New Dependencies

| Crate | For | Decision |
|---|---||
| `xxhash-rust` **(foundation)** | Hash computation | xxHash32 for fast 2-character line hashes. Alternative: implement simple hash function to avoid dependency. **Decision: Implement custom hash to minimize dependencies.** |

**Rationale for custom hash:**
- xxHash32 adds ~150KB to binary size
- Simple 2-character hash is sufficient for collision avoidance in this use case
- Reference implementations use custom alphabets and seeding strategies
- Easier to port and test

## Decisions

**Format choice:** `LINE#HASH:content` with left-padded line numbers for column alignment, matching `pi-hashline-edit`. The `#` separator and 2-character hash from alphabet `ZPMQVRWSNKTXJBYH` (excludes hex digits, vowels, and visually ambiguous letters like D/G/I/L/O).

Example:
```text
 8#VR:function hello() {
 9#KT:  console.log("world");
10#BH:}
```

**Line number padding:** Dynamic padding based on file line count (e.g., ` %3d#` for 999-line files). Ensures the hash column aligns visually across all lines.

**Backward compatibility:** `EditFile` accepts both formats transparently:
- Hashline format: `{op: "replace", pos: "9#KT", lines: [...]}`
- Legacy format: `{old_text: "...", new_text: "..."}` (exact-match, current behavior)

Detection based on presence of `"op"` field in each edit item.

**Opt-out mechanism:** `ReadFile` accepts `hashline: false` parameter to return plain `<context>` format for users who prefer legacy behavior or are working with tools that expect plain text.

**Hash computation strategy:** 
- For lines with alphanumeric characters: hash based on line content
- For lines without alphanumeric characters (e.g., `}`, `]`): hash based on line number as seed
- This reduces collisions on structurally identical markers

**TUI consideration (Phase 4):** Hashline output is TUI-compatible. The format can be parsed to extract line numbers, hashes, and content separately. TUI can render with optional hash visibility (dimmed by default, toggle with `h` key).

## Exit Criteria

The agent reads files with hashline output by default, edits using hash anchors, and recovers gracefully from stale context with helpful error messages. Legacy exact-match edits continue to work. TUI integration (Phase 4) can leverage the structured format for rich rendering.