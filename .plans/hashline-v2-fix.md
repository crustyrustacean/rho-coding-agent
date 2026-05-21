# Hashline v2: Fix Plan

**Date:** 2026-05-20
**Motivation:** Real-world session analysis (gpt-4o, 102400K context) revealed systemic friction in the hashline editing system, causing repeated model round-trips on stale anchors and a near-meaningless hash due to collision saturation.

---

## Problem Statement

Phase 3.10 shipped hashline editing with a 2-character hash from a 16-character alphabet (256 possible values). Three categories of problem emerged in production use:

### P1: Hash Collision Saturation (Critical)

The 2-char hash provides only **256 possible values**. By the birthday problem, a file with just 20 lines has a ~54% chance of at least one collision. A 106-line Rust file produced **23 collisions** (22% of all lines). The hash is not discriminating — it provides a false sense of safety while failing to uniquely identify lines.

In the session logs, the model's anchors were technically "correct" (right line number, right hash) but the hash matched *other lines too*, meaning the hash provided zero additional disambiguation beyond the line number alone.

### P2: Stale Anchor Retry Spiral (High)

When the model makes edit A, the file changes, invalidating the anchors the model received from its prior `read_file`. Edit B then fails with a hash mismatch. The model retries with fresh anchors (1 round-trip), makes edit C with stale anchors from the fresh ones (another mismatch, another retry). This compounds: 5 edits → up to 5 retries → 10 total model calls instead of 5.

The system *does* return fresh hashes in the diff output after each edit (`Note: Anchors in diff are fresh. Use for chained edits.`), but the model (gpt-4o) does not reliably use them. It re-reads the original `read_file` output from context and re-derives anchors from the stale data.

### P3: Diff Format Bug (Medium)

`format_hashline_diff` has a display bug for modified lines — both the `-` and `+` lines show `line_content` from `new_lines[i]`, making the diff visually identical for old and new. The model sees:

```
-  55#JY:        let metadata_json = metadata.map(|m| serde_json::to_string(m)).transpose()?;
+  55#XX:        let metadata_json = metadata.map(|m| serde_json::to_string(m)).transpose()?;
```

The `-` line should show the *old* content (before the edit), not the new content.

---

## Root Cause Analysis

| Problem | Root Cause | Owner |
|---------|-----------|-------|
| P1: Collision saturation | Hash space too small (256 values) | Tooling |
| P2: Stale anchor retries | Hard-fail on any hash mismatch, no fuzzy fallback | Tooling |
| P3: Diff bug | `format_hashline_diff` shows `new_lines[i]` for both old and new | Tooling (bug) |

All three are fixable in `rho-tools/src/files.rs` and `rho-tools/src/hashline.rs`. No changes to `rho-core` or other crates are needed.

---

## Fix Plan

### Fix 1: Expand Hash to 4 Characters (P1)

**File:** `rho-tools/src/hashline.rs`
**Impact:** Reduces collision space from 256 to 65,536. For a 106-line file, P(collision) drops from ~100% to ~7.8%. For a 500-line file, from ~100% to ~85% (still not zero, but vastly better). For files under ~250 lines (the majority of edits), collision probability is under 50%.

**Changes:**

```rust
// Before:
const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH"; // 16 chars → 2-char hash → 256 values

// After: 
const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH"; // 16 chars → 4-char hash → 65,536 values

pub fn compute_line_hash(line: &str, line_num: usize) -> String {
    let seed = if line.chars().any(char::is_alphanumeric) {
        line.bytes().fold(0u32, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u32::from(b))
        })
    } else {
        u32::try_from(line_num).unwrap_or(u32::MAX)
    };

    // Use 4 indices instead of 2
    let idx1 = (seed & 0x0F) as usize;
    let idx2 = ((seed >> 4) & 0x0F) as usize;
    let idx3 = ((seed >> 8) & 0x0F) as usize;
    let idx4 = ((seed >> 12) & 0x0F) as usize;

    let bytes = [ALPHABET[idx1], ALPHABET[idx2], ALPHABET[idx3], ALPHABET[idx4]];
    String::from_utf8(bytes.to_vec()).expect("ALPHABET contains valid UTF-8")
}
```

**But wait** — with only 16 bits of seed (we're masking to 4×4 bits = 16 bits), a 4-char hash from a 16-char alphabet gives us 65,536 *possible* values but only 65,536 *actual* outputs (since the seed itself is 32 bits but we only use the bottom 16 bits). This means the 4-char hash is actually a perfect representation of the seed's bottom 16 bits. For better dispersion, we should use more of the seed:

```rust
pub fn compute_line_hash(line: &str, line_num: usize) -> String {
    let seed = if line.chars().any(char::is_alphanumeric) {
        line.bytes().fold(0u32, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(u32::from(b))
        })
    } else {
        // Mix line number more thoroughly for non-alphanumeric lines
        let n = u32::try_from(line_num).unwrap_or(u32::MAX);
        n.wrapping_mul(2654435761) // Knuth multiplicative hash
    };

    // Fold the 32-bit seed into 4 indices using all 32 bits
    let idx1 = (seed & 0x0F) as usize;
    let idx2 = ((seed >> 4) & 0x0F) as usize;
    let idx3 = ((seed >> 8) & 0x0F) as usize;
    let idx4 = ((seed >> 12) & 0x0F) as usize;

    let bytes = [ALPHABET[idx1], ALPHABET[idx2], ALPHABET[idx3], ALPHABET[idx4]];
    String::from_utf8(bytes.to_vec()).expect("ALPHABET contains valid UTF-8")
}
```

**Backward compatibility:** The anchor format changes from `9#KT` to `9#KTNS`. This is a **breaking change** for any in-flight sessions with 2-char anchors. Mitigations:
- Bump the anchor format version in the `EditFile` description
- Accept both 2-char and 4-char anchors during a transition period (detect by length after `#`)
- Update the regex for TUI parsing: `^(\s*)(\d+)#([A-Z]{2,4}):(.*)$`

**Token cost:** 4-char hash adds 2 characters per line. For a 500-line file, that's 1000 extra characters ≈ ~250 tokens. Negligible in a 102400K context window.

**Task breakdown:**
- [ ] Update `compute_line_hash` to produce 4-char hashes
- [ ] Update `HashlineAnchor::parse` to accept 2-4 char hashes (backward compat)
- [ ] Update `validate_hashline_edits` to handle both lengths
- [ ] Update all tests
- [ ] Update TUI regex documentation

### Fix 2: Fuzzy Anchor Matching on Hash Mismatch (P2)

**File:** `rho-tools/src/files.rs` (method `validate_hashline_edits` on `EditFile`)
**Impact:** Eliminates the retry spiral. Instead of hard-failing on hash mismatch, the system applies a tiered verification and only fails when the line is truly unrecognizable.

**Current behavior:**
```
hash mismatch at anchor 55#WM
Expected:  55#WM:  let metadata_json = metadata.map(|m| serde_json::to_string(m)).transpose()?;
Actual:    55#JY:  let metadata_json = metadata.map(|m| serde_json::to_string(m)).transpose()?;
Use updated anchor 55#JY to retry.
```
→ Model must retry (1 round-trip lost).

**New behavior (tiered validation):**

```
Tier 1: Hash matches → Apply silently (current behavior, no change)

Tier 2: Hash mismatches, line number valid, content structurally similar
  → Apply with WARNING (new)
  → Return result includes:
    "applied 1 hashline edit(s) with anchor relaxation:
     anchor 55#WM → actual 55#JY (content matched at line 55)
     Warning: hash was stale. Re-read file if further edits needed."

Tier 3: Hash mismatches, line number valid, content is different
  → Search ±5 lines for a line matching the original content pattern
  → If found: Apply at the found line with WARNING
  → If not found: Hard fail with fresh hashes (current behavior)
```

**"Structurally similar" heuristic:**
The key insight is that when the hash mismatches due to a *prior edit*, the content at the target line is often *close* to what the model expects. We can check:
1. Levenshtein distance on the first 40 characters of the line (after stripping whitespace)
2. If distance ≤ 5 characters, consider it a match
3. Also: if the line shares the same first token (e.g., `let`, `fn`, `pub`, `use`), consider it a candidate

**Implementation:**

```rust
fn content_similarity(a: &str, b: &str) -> usize {
    // Simple character-level similarity after stripping leading whitespace
    let a = a.trim_start();
    let b = b.trim_start();
    
    if a == b { return usize::MAX; } // Perfect match
    
    // Check if first "word" (up to first space/punct) matches
    let first_word = |s: &str| s.split(|c: char| c.is_whitespace() || c == '(' || c == ':').next().unwrap_or("");
    if first_word(a) == first_word(b) && !first_word(a).is_empty() {
        return 10; // Same leading token — likely the right line
    }
    
    0 // No similarity
}
```

For Tier 3 (neighborhood search), walk ±5 lines and find the best-matching line by the same similarity metric.

**Important constraint:** Fuzzy matching must NOT be used when the line content is very short (e.g., `}`, `]`, empty lines). These lines have low information content and matching by content similarity would produce false positives. For such lines, fall through to hard-fail with fresh hashes.

**Task breakdown:**
- [ ] Implement `content_similarity` function
- [ ] Add tiered validation to `validate_hashline_edits`
- [ ] Add "applied with anchor relaxation" output format
- [ ] Add tests for each tier (exact match, fuzzy match, neighborhood search, hard fail)
- [ ] Test that truly wrong edits still hard-fail (safety)

### Fix 3: Diff Format Bug (P3)

**File:** `rho-tools/src/files.rs` (method `format_hashline_diff`)
**Impact:** Corrects confusing diff output that showed identical content for old and new lines.

**Current (buggy) code:**
```rust
if is_changed && i < old_lines.len() {
    // Modified line — show both old and new
    w(&format!("- {line_num:>width$}#{hash}:{line_content}\n"));
    let old_hash = compute_line_hash(old_lines[i], line_num);
    w(&format!("+ {line_num:>width$}#{old_hash}:{line_content}\n"));
}
```

Both lines use `line_content` (from `new_lines[i]`). The `-` line should show the old content.

**Fix:**
```rust
if is_changed && i < old_lines.len() {
    // Modified line — show old then new
    let old_content = old_lines[i];
    let old_hash = compute_line_hash(old_content, line_num);
    w(&format!("- {line_num:>width$}#{old_hash}:{old_content}\n"));
    w(&format!("+ {line_num:>width$}#{hash}:{line_content}\n"));
}
```

**Task breakdown:**
- [ ] Fix the diff output to show old content on `-` lines
- [ ] Add test verifying diff shows different content for old vs new lines

### Fix 4: Improved Anchor Refresh After Edits (P2, supplementary)

**File:** `rho-tools/src/files.rs` (method `apply_hashline_edits`)
**Impact:** Makes the fresh-anchor output more prominent and actionable, reducing the chance the model ignores it.

**Current behavior:** After a successful edit, the diff is appended with `Note: Anchors in diff are fresh. Use for chained edits.` This note is easy to miss in a long diff.

**Improvement:** Add a structured block at the end of every edit result:

```
applied 2 hashline edit(s) to backend/src/db.rs

<diff>
...diff content...
</diff>

<fresh-anchors>
  53#ZT:        let now = Utc::now().to_rfc3339();
  54#KS:        let tags_json = serde_json::to_string(tags)?;
  55#JY:        let metadata_json = metadata.map(serde_json::to_string).transpose()?;
  56#MP:
  57#WN:        let result = sqlx::query(
</fresh-anchors>
Lines 53-57 have fresh anchors. Use these for subsequent edits to this region.
```

This gives the model an explicit, easy-to-parse block of fresh anchors without needing to re-read the file.

**Task breakdown:**
- [ ] Add `<fresh-anchors>` block to edit result output
- [ ] Include ±5 lines around each edit region
- [ ] Add tests for the new output format

---

## What NOT To Change

1. **The legacy `old_text`/`new_text` edit format** — It works well and serves as the fallback.
2. **The `<context>` framing** — Security feature, untouchable.
3. **The tree-sitter node-splitting validation** — Orthogonal to this fix.
4. **The agent loop** — All changes are tool-level (`rho-tools` only).
5. **The system prompt hashline instructions** — Will need minor updates for 4-char hashes but the structure is sound.

---

## Migration Strategy

Since anchors are ephemeral (they exist only within a conversation turn), the migration is straightforward:

1. **Phase A (non-breaking):** Ship fuzzy matching (Fix 2), diff fix (Fix 3), and anchor refresh (Fix 4). These are purely additive — the system accepts the same 2-char anchors but handles them more gracefully.

2. **Phase B (breaking):** Ship 4-char hashes (Fix 1). This changes the output format of `read_file` and the expected anchor format. Any in-flight sessions will have stale 2-char anchors that won't match 4-char hashes. Mitigation:
   - During Phase B, `EditFile` accepts both 2-char and 4-char anchors
   - 2-char anchors are validated against the legacy hash function
   - After ~2 weeks (or next release), remove 2-char backward compatibility

This ordering means we get immediate relief from the retry spiral (Phase A) before taking the breaking change (Phase B).

---

## Estimated Effort

| Fix | Effort | Priority |
|-----|--------|----------|
| Fix 1: 4-char hash | 3-4 hours | P1 (Phase B) |
| Fix 2: Fuzzy matching | 4-6 hours | P1 (Phase A) |
| Fix 3: Diff bug | 1 hour | P2 (Phase A) |
| Fix 4: Anchor refresh | 2-3 hours | P2 (Phase A) |
| Tests | 4-5 hours | P1 |
| **Total** | **14-19 hours** | |

---

## Success Criteria

1. **No hash collisions** in files up to ~200 lines (Fix 1)
2. **Zero retry-round-trips** when a prior edit in the same session changed the target file (Fix 2)
3. **Correct diff display** — `-` lines show old content, `+` lines show new (Fix 3)
4. **All existing tests pass** — no regressions
5. **Eval suite** passes with same or better scores

## Validation

After implementing, replay the problematic session against the knowledge-base project:
1. Read `backend/src/db.rs`
2. Apply 5 chained edits (the same ones from the session log)
3. Measure: how many retries were needed? (Target: 0)
4. Verify: do all edits apply correctly? (Target: yes)
