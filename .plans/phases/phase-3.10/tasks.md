# Phase 3.10 Tasks

**Status:** 🔜 In Planning

---

1. **Add hash computation module to `rho-tools`**
   - Create new module: `rho-tools/src/hashline.rs`
   - Implement `compute_line_hash(line: &str, line_num: usize) -> &'static str`
   - Use custom alphabet: `const ALPHABET: &[u8] = b"ZPMQVRWSNKTXJBYH"`
   - Hash logic:
     ```rust
     let seed = if line.chars().any(|c| c.is_alphanumeric()) {
         // Simple hash based on line content
         line.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32))
     } else {
         // Use line number for lines without alphanumerics (e.g., }{)()})
         line_num as u32
     };
     let idx1 = (seed & 0x0F) as usize;
     let idx2 = ((seed >> 4) & 0x0F) as usize;
     unsafe { std::str::from_utf8_unchecked(&[ALPHABET[idx1], ALPHABET[idx2]]) }
     ```
   - Add unit tests for:
     - Deterministic output for same input
     - Different hashes for different lines
     - Line-number-based hashing for non-alphanumeric lines
     - Alphabet coverage (all 16 chars used with varied input)

2. **Extend `ReadFile` to output hashline format**
   - Add optional parameter: `hashline: bool` (default: `true`)
   - When `hashline: true`, format each line as: `{line_num}#{hash}:{content}`
   - Compute line number padding based on total line count:
     ```rust
     let width = line_count.to_string().len();
     format!("{:#width$}#{}:{}", line_num, hash, line, width = width)
     ```
   - Wrap in existing `<context>` framing:
     ```text
     <context>
      8#VR:function hello() {
      9#KT:  console.log("world");
     10#BH:}
     <context:end>
     ```
   - When `hashline: false`, return legacy format (plain `<context>` wrapper)
   - Update `parameters_schema()` to include optional `hashline` field
   - Update `description()` to document hashline format
   - Handle edge cases:
     - Empty files: Return advisory message suggesting `write_file` instead of synthetic anchor
     - Binary files: Reject with descriptive error (same as current behavior)
     - Very long lines: Continue without truncation (model sees full content)
   - Add integration tests:
     - Read file with hashline enabled
     - Read file with hashline disabled
     - Verify line number padding for various file sizes
     - Verify hash computation matches expectations

3. **Extend `EditFile` to support hashline edits**
   - Modify `parameters_schema()` to support both formats:
     - Legacy: `{"path": "...", "edits": [{"old_text": "...", "new_text": "..."}]}`
     - Hashline: `{"path": "...", "edits": [{"op": "replace", "pos": "9#KT", "lines": ["..."]}]}`
   - Add hashline edit operations:
     - `replace`: Replace line at anchor `pos` (or range with `end`)
     - `append`: Insert lines after anchor `pos`
     - `prepend`: Insert lines before anchor `pos`
     - `delete`: Delete line at anchor `pos` (or range with `end`)
   - Implement format detection in `execute()`:
     ```rust
     for edit in edits {
         if let Some(op) = edit["op"].as_str() {
             apply_hashline_edit(...)?;
         } else {
             apply_legacy_edit(...)?;
         }
     }
     ```
   - Hashline edit logic:
     - Parse anchor: `"9#KT"` → `line_num=9, hash="KT"`
     - Read current file content
     - Validate hash: compute hash for line `line_num`, compare with expected
     - On mismatch: return error with fresh hashes for affected region
     - On match: apply edit operation
   - Edit operation implementation:
     - `replace single`: Swap line content at anchor
     - `replace range`: Swap lines from `pos` anchor to `end` anchor
     - `append`: Insert lines after anchor (EOF if omitted)
     - `prepend`: Insert lines before anchor (BOF if omitted)
     - `delete`: Remove line(s) at anchor
   - Apply edits bottom-up (from last line to first) to preserve line numbers
   - Add validation:
     - Hash must match exactly (no fuzzy matching)
     - Operations must not overlap
     - Line numbers must be valid (1 to file line count)
   - Maintain existing validation for legacy edits (uniqueness, no overlap)
   - Add unit tests:
     - Replace single line with valid hash
     - Replace range with valid hashes
     - Append/prepend operations
     - Delete operations
     - Hash mismatch errors
     - Invalid anchor format
     - Out-of-bounds line numbers

4. **Implement hash mismatch error recovery**
   - When hash mismatch detected, compute fresh hashes for affected region
   - Return error message with:
     - Anchor that failed
     - Expected line with old hash
     - Actual line with new hash
     - Suggestion: "Use updated anchor {line_num}#{new_hash} to retry"
   - Example error:
     ```
     edit_file: hash mismatch at anchor 9#KT
     Expected line:  9#KT:  console.log("world");
     Actual line:    9#RT:  console.log("updated world");
     
     Use updated anchor 9#RT to retry.
     ```
   - For range operations, show mismatch for first failing anchor
   - Include fresh hashes for ±3 lines around mismatch for context

5. **Implement diff generation for edit results**
   - After successful edit, compute unified diff between old and new content
   - Format with hashline anchors:
     ```text
     <diff>
     --- /tmp/file.txt
     +++ /tmp/file.txt
      3#NQ:line 3
      4#RH:line 4
      5#XH:line 5
      6#ZT:line 6
     - 8#  :line 8      ← deleted (no hash, 2-space padding)
     + 8#RT:modified line 8  ← added (fresh hash)
      9#PJ:line 9
     10#NV:line 10
     
     Note: Lines after edited regions have stale hashes. Use read_file to refresh.
     </diff>
     ```
   - Show ±5 lines of context around each change
   - Use `...` for gaps between change regions
   - Append terse note about stale hashes for chained edits
   - Include diff in `ToolResult` output

6. **Implement chained edit support**
   - After successful edit, include updated anchors in result
   - Format:
     ```text
     --- Updated anchors ---
      8#RT:modified line 8
      9#PJ:line 9
     10#NV:line 10
     ```
   - Allow model to use these anchors for immediate next edit without full re-read
   - Document limitation: Only valid for nearby changes; distant edits should re-read file
   - Return affected line range (first_changed_line to last_changed_line) for context

7. **Update tool descriptions and system prompts**
   - Update `ReadFile.description()` to explain hashline format and `hashline` parameter
   - Update `EditFile.description()` to document both hashline and legacy formats
   - Update base system prompt with hashline usage:
     ```
     When reading files, use read_file with hashline enabled (default).
     Each line is prefixed with LINE#HASH: (e.g., " 9#KT:console.log('hello')").
     When editing, use these hash anchors instead of quoting full lines.
     Edits specify: {op: "replace", pos: "9#KT", lines: ["new content"]}.
     If the file has changed, you'll receive an error with fresh hashes to retry.
     ```
   - Add examples to prompts:
     - Read then edit workflow
     - Handling hash mismatch errors
     - Chained edits using updated anchors

8. **Add backward compatibility tests**
   - Ensure legacy `old_text`/`new_text` edits continue to work
   - Test mixed edit arrays (some hashline, some legacy)
   - Verify `hashline: false` parameter returns plain format
   - Test with existing eval scenarios (rho-eval tasks)
   - Ensure no regression in current functionality

9. **Add comprehensive error handling tests**
   - Test all error conditions:
     - Invalid anchor format (missing `#`, wrong format)
     - Hash mismatch (file changed)
     - Out-of-bounds line numbers
     - Overlapping edits (hashline)
     - Missing required fields per operation
     - Empty lines array for operations that require it
   - Verify error messages are clear and actionable
   - Test error recovery (model can retry with fresh anchors)

10. **Integration tests for end-to-end hashline workflow**
    - Scenario: Read file → edit with hashline anchor → verify edit applied
    - Scenario: Read file → wait for external change → edit → hash mismatch error → retry with fresh anchor
    - Scenario: Chained edits using updated anchors from previous result
    - Scenario: Mixed hashline and legacy edits in same call
    - Scenario: Multi-file edits with hashline
    - Test with various file types (Rust, TOML, JSON, Markdown, PowerShell)
    - Test with large files (1000+ lines) to verify padding

11. **Documentation updates**
    - Update `AGENTS.md` with hashline usage examples
    - Update `README.md` to mention hashline editing feature
    - Add inline documentation to `hashline.rs` module
    - Document custom hash algorithm in comments
    - Update `CHANGELOG.md` with feature summary

12. **Performance validation**
    - Benchmark hash computation for typical file sizes
    - Verify hashline format doesn't significantly impact token usage
    - Compare model token usage with vs without hashline (should be lower with hashline)
    - Test with very large files (10k+ lines) to ensure acceptable performance

13. **TUI preparation (for Phase 4)**
    - Design hashline parsing utilities for TUI integration
    - Document format for TUI rendering:
      - Line numbers and hashes can be extracted with regex: `^\s*(\d+)#([A-Z]{2}):(.*)$`
      - Hash column can be dimmed or hidden
      - Anchor parsing for click-to-copy functionality
    - Add helper function (internal) to strip hashes if needed:
      ```rust
      fn strip_hashes(hashline_content: &str) -> String {
          hashline_content.lines()
              .map(|line| line.splitn(3, ':').nth(2).unwrap_or(line))
              .collect::<Vec<_>>()
              .join("\n")
      }
      ```

14. **Feature flag consideration**
    - Evaluate whether to make hashline feature-gated
    - Current decision: Default enabled, opt-out via parameter
    - Document rationale: Hashline is core safety feature, not experimental

---

## Testing Strategy

### Unit Tests
- Hash computation (`compute_line_hash`)
- Anchor parsing
- Edit operation logic (replace, append, prepend, delete)
- Diff generation
- Error formatting

### Integration Tests
- ReadFile with hashline enabled/disabled
- EditFile with hashline anchors
- Hash mismatch error recovery
- Backward compatibility with legacy format
- Mixed format arrays

### End-to-End Tests (via rho-eval)
- New eval scenario: Fix stale-context bug using hashline
- Verify existing scenarios still pass with hashline default
- Compare token usage with/without hashline

### Regression Tests
- All existing tests must pass
- No breaking changes to legacy format
- eval pass rate must not decrease

---

## Success Criteria

1. ✅ `ReadFile` outputs hashline format by default with `LINE#HASH:` prefix
2. ✅ `EditFile` accepts and validates hash anchors
3. ✅ Hash mismatches fail with helpful error messages containing fresh hashes
4. ✅ Legacy `old_text`/`new_text` edits continue to work
5. ✅ All existing tests pass (no regressions)
6. ✅ New tests cover hashline-specific functionality
7. ✅ System prompts updated with hashline usage
8. ✅ Documentation updated (AGENTS.md, README.md, CHANGELOG.md)
9. ✅ eval scenarios validate improved edit reliability
10. ✅ TUI can parse and render hashline output (documented for Phase 4)