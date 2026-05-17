# Phase 3.5 Tasks 🔜 Planned

**Estimated effort:** 2–4 days

Full plan in Obsidian: [[Phase 3.5 — Rust Standard Library Reference]]
Implementation proposal: [proposal.md](./proposal.md)

---

### Task 1: Doc Root Discovery and Caching

**File:** `rho-tools/src/rust/rustdoc.rs` (new file)

- [ ] Add `struct RustdocTool { doc_root: PathBuf }`
- [ ] In `new()`, run `rustup doc --path` via `std::process::Command` to discover the HTML root
- [ ] Cache the path; panic if `rustup` not found
- [ ] No `ShellExecutor` field — the tool only reads files at runtime, no shell commands needed

**Tests:**
- Doc root exists after construction
- Panic when `rustup` not found (tempdir with no rustup)

### Task 2: Query Resolution

- [ ] Implement `resolve_query(query: &str) -> Option<PathBuf>`
- [ ] Handle: bare type names, fully-qualified paths, primitive types
- [ ] Support `std`, `core`, `alloc` crate prefixes
- [ ] Map `char` → `std/primitive.char.html`, `str` → `std/primitive.str.html`, etc.
- [ ] Hard-coded lookup table for common types (Vec, Option, Result, String, Box, Rc, Arc, HashMap, HashSet, Mutex, RwLock, etc.)
- [ ] Fallback: walk `sidebar-items*.js` for unconventional names

**Known path patterns:**
| Pattern | Example | File |
|---|---|---|
| `struct.XXX` | `Vec` | `std/vec/struct.Vec.html` |
| `enum.XXX` | `Option` | `core/option/enum.Option.html` |
| `trait.XXX` | `Display` | `std/fmt/trait.Display.html` |
| `fn.XXX` | `mem::swap` | `std/mem/fn.swap.html` |
| `macro.XXX` | `vec!` | `std/macro.vec.html` |
| `primitive.XXX` | `str` | `std/primitive.str.html` |
| `const.XXX` | `MAX` | `std/usize/constant.MAX.html` |

**Tests:**
- `Vec` → `std/vec/struct.Vec.html`
- `std::collections::HashMap` → `std/collections/struct.HashMap.html`
- `Option` → `core/option/enum.Option.html`
- `Display` → `std/fmt/trait.Display.html`
- `str` → `std/primitive.str.html`
- `NonExistentType123` → `None`

### Task 3: HTML Stripping (V1)

- [ ] Read the resolved HTML file
- [ ] Extract `<main>` or `<section id="main-content">` content only (skip nav, sidebar, footer)
- [ ] Strip tags using a simple state machine (no new deps)
- [ ] Decode HTML entities (`&amp;` → `&`, `&lt;` → `<`, `&gt;` → `>`, `&quot;` → `"`, numeric entities)
- [ ] Collapse runs of whitespace into single newlines

**Tests:**
- Extracts description text from `Vec` page
- Extracts method signatures from `Vec` page
- Extracts code examples from `Vec` page
- Handles missing/malformed HTML gracefully
- `<p>Hello <em>world</em></p>` → `Hello world`
- `&amp; &lt; &gt;` → `& < >`

### Task 4: Section Filtering

- [ ] When `section` parameter is specified, extract only the relevant portion:
  - `"all"` → Everything
  - `"methods"` → Method signatures and brief descriptions (under "Implementations" / "Methods" headings)
  - `"traits"` → Trait implementation list (under "Trait Implementations" heading)
  - `"examples"` → Code examples only (`<pre><code>` blocks)
  - `"signature"` → Type declaration line only (first paragraph)

**Tests:**
- Each section value returns the correct subset
- `"all"` returns everything
- Invalid section value returns error

### Task 5: Registration and System Prompt

- [ ] Add `mod rustdoc` and `pub use rustdoc::RustdocTool` to `rho-tools/src/rust/mod.rs`
- [ ] Add `RustdocTool` to `pub use` line in `rho-tools/src/lib.rs`
- [ ] Register `RustdocTool::new()` in `register_all()` with risk `Read` (no SandboxRoot needed)
- [ ] Add prompt guidance to `rho-core/src/prompts/base.md` under "Working with Rust code"

**Tests:**
- Tool registered with correct name `rustdoc_lookup` and risk `Read`
- Integration test: tool appears in registry

### Task 6: Evaluation Scenario

- [ ] Add scenario: "Look up the `std::sync::Mutex` API and write a correct implementation of a shared counter using `Arc<Mutex<usize>>`. Verify it compiles."

### Task 7: CI Verification

- [ ] Run `cargo xtask ci` and ensure all checks pass
- [ ] Verify ≥ 10 unit tests in `rustdoc.rs`
