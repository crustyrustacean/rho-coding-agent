# Phase 3.5 Tasks 🔜 Planned

**Estimated effort:** 2–4 days

Full plan in Obsidian: [[Phase 3.5 — Rust Standard Library Reference]]

---

### Task 1: Doc Root Discovery and Caching

**File:** `rho-tools/src/rustdoc.rs`

- [ ] Add `struct RustdocTool { root: PathBuf, shell: Box<dyn ShellExecutor> }`
- [ ] In `new()`, run `rustup doc --path` to discover the HTML root
- [ ] Cache the path; panic if `rustup` not found

**Tests:**
- Doc root exists after construction
- Panic when `rustup` not found (tempdir with no rustup)

### Task 2: Query Resolution

- [ ] Implement `resolve_query(query: &str) -> Option<PathBuf>`
- [ ] Handle: bare type names, fully-qualified paths, primitive types
- [ ] Support `std`, `core`, `alloc` crate prefixes
- [ ] Map `char` → `std/primitive.char.html`, `str` → `std/primitive.str.html`, etc.
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
- [ ] Strip tags using regex or simple state machine
- [ ] Extract `<main>` content only (skip nav, sidebar, footer)
- [ ] Decode HTML entities (`&amp;` → `&`, `&lt;` → `<`, etc.)

**Tests:**
- Extracts description text from `Vec` page
- Extracts method signatures from `Vec` page
- Extracts code examples from `Vec` page
- Handles missing/malformed HTML gracefully

### Task 4: Section Filtering

- [ ] When `section` parameter is specified, extract only the relevant portion:
  - `"all"` → Everything
  - `"methods"` → Method signatures and brief descriptions
  - `"traits"` → Trait implementation list
  - `"examples"` → Code examples only
  - `"signature"` → Type declaration line only

**Tests:**
- Each section value returns the correct subset
- `"all"` returns everything
- Invalid section value returns error

### Task 5: Registration and System Prompt

- [ ] Register `RustdocTool` in `register_all()` with risk `Read`
- [ ] Add tool description to system prompt / base prompt
- [ ] The model should know when to use `rustdoc_lookup` vs `rustc_explain`

**Tests:**
- Tool registered with correct name and risk
- Integration test: model can use the tool in a prompt scenario

### Task 6: Evaluation Scenario

- [ ] Add scenario: "Look up the `std::sync::Mutex` API and write a correct implementation of a shared counter using `Arc<Mutex<usize>>`. Verify it compiles."
