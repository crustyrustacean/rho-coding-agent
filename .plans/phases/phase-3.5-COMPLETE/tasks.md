# Phase 3.5 Tasks ✅ Complete

**Completed:** 2026-05-17

Full plan in Obsidian: [[Phase 3.5 — Rust Standard Library Reference]]
Implementation proposal: [proposal.md](./proposal.md)

---

### Task 1: Doc Root Discovery and Caching

**File:** `rho-tools/src/rust/rustdoc.rs` (new file)

- [x] Add `struct RustdocTool { doc_root: PathBuf }`
- [x] In `new()`, run `rustup doc --path` via `std::process::Command` to discover the HTML root
- [x] Cache the path; panic if `rustup` not found
- [x] No `ShellExecutor` field — the tool only reads files at runtime, no shell commands needed

### Task 2: Query Resolution

- [x] Implement `resolve_query(query: &str) -> Option<PathBuf>`
- [x] Handle: bare type names, fully-qualified paths, primitive types
- [x] Support `std`, `core`, `alloc` crate prefixes
- [x] Map `char` → `std/primitive.char.html`, `str` → `std/primitive.str.html`, etc.
- [x] Hard-coded lookup table for common types (Vec, Option, Result, String, Box, Rc, Arc, HashMap, HashSet, Mutex, RwLock, etc.)
- [x] Fallback: walk `sidebar-items*.js` for unconventional names

### Task 3: HTML Stripping (V1)

- [x] Read the resolved HTML file
- [x] Extract `<main>` or `<section id="main-content">` content only (skip nav, sidebar, footer)
- [x] Strip tags using a simple state machine (no new deps)
- [x] Decode HTML entities (`&amp;` → `&`, `&lt;` → `<`, `&gt;` → `>`, `&quot;` → `"`, numeric entities)
- [x] Collapse runs of whitespace into single newlines

### Task 4: Section Filtering

- [x] When `section` parameter is specified, extract only the relevant portion:
  - `"all"` → Everything
  - `"methods"` → Method signatures and brief descriptions
  - `"traits"` → Trait implementation list
  - `"examples"` → Code examples only
  - `"signature"` → Type declaration line only

### Task 5: Registration and System Prompt

- [x] Add `mod rustdoc` and `pub use rustdoc::RustdocTool` to `rho-tools/src/rust/mod.rs`
- [x] Add `RustdocTool` to `pub use` line in `rho-tools/src/lib.rs`
- [x] Register `RustdocTool::new()` in `register_all()` with risk `Read` (no SandboxRoot needed)
- [x] Add prompt guidance to `rho-core/src/prompts/base.md` under "Rust Tooling"

### Task 6: Evaluation Scenario

- [x] Add scenario 06: HashMap::get return-type confusion — model must use `rustdoc_lookup` to understand `Option` return type and fix the code
- [x] Three unit tests: entry API fix, unwrap_or fix, original bug fails
- [x] Passes against Gemma 4 27B (53s, 8 iterations)

### Task 7: CI Verification

- [x] Run `cargo xtask ci` — all checks pass
- [x] 37 unit tests in `rustdoc.rs` (exceeds ≥ 10 target)
- [x] 689 total tests pass across the full workspace

---

## Bonus fixes delivered alongside

- `max_tokens` field added to `ChatRequest` — sent as `completion_reserve` (4096) so thinking models get explicit output budget from the server.
- `EntryId` widened from 32-bit to 64-bit to eliminate UUID collision flake in CI.
