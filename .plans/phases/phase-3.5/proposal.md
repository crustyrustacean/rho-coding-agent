# Phase 3.5: Implementation Proposal — Rustdoc Lookup Tool

**Status:** Approved
**Date:** 2026-05-17

---

## Design decision: no `ShellExecutor` in `RustdocTool`

The plan calls for `struct RustdocTool { root: PathBuf, shell: Box<dyn ShellExecutor> }` and running `rustup doc --path` in `new()`. But `ShellExecutor::execute` is async, and `new()` is synchronous. Two options were considered:

- **Option A (chosen):** Use `std::process::Command` in `new()` for the one-time `rustup doc --path` discovery. The `shell` field is unnecessary — all subsequent work is `std::fs::read_to_string` on HTML files. Simplest approach, matches the plan's "cache the path; panic if rustup not found" intent.
- **Option B:** Keep `shell` and use a lazy `OnceCell<PathBuf>` populated on first `execute()` call. Adds async complexity for no real benefit.

The struct becomes `RustdocTool { doc_root: PathBuf }`. No `ShellExecutor` needed. The tool's `execute()` method only reads files — no shell commands at runtime.

## Design decision: file location

The plan says `rho-tools/src/rustdoc.rs`, but existing Rust tools live under `rho-tools/src/rust/`. Placing the new file at `rho-tools/src/rust/rustdoc.rs` stays consistent with the existing module layout.

## Design note: truncation

The plan mentions "apply the same `ToolResultDetails::FullOutput` truncation pattern used by `CargoCheck`." In practice, `CargoCheck` uses `ToolResultDetails::Diagnostics`, not `FullOutput`. The `FullOutput` variant is created by the **session** during truncation when a tool result exceeds the token budget — not by individual tools. No special truncation logic is needed in the tool itself. The tool returns raw output and the session handles truncation automatically.

---

## Files to change

### 1. `rho-tools/src/rust/rustdoc.rs` — NEW (~250–300 lines)

The core implementation with three main concerns.

#### Doc root discovery

```rust
pub struct RustdocTool {
    doc_root: PathBuf,
}

impl RustdocTool {
    pub fn new() -> Self {
        let output = std::process::Command::new("rustup")
            .args(["doc", "--path"])
            .output()
            .expect("rustup not found — cannot discover rustdoc root");
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let doc_root = PathBuf::from(path);
        assert!(
            doc_root.exists(),
            "rustdoc root does not exist: {}",
            doc_root.display()
        );
        Self { doc_root }
    }
}
```

#### Query resolution (`resolve_query`)

Parse the `query` string and map it to a relative HTML path under `doc_root`:

| Query form | Resolution |
|---|---|
| `Vec` (bare type) | `std/vec/struct.Vec.html` |
| `std::collections::HashMap` (fully-qualified) | `std/collections/struct.HashMap.html` |
| `Option::map` (method) | Resolve `Option` → `core/option/enum.Option.html`, then extract method section from HTML |
| `Display` (trait) | `std/fmt/trait.Display.html` |
| `str` (primitive) | `std/primitive.str.html` |
| `mem::swap` (function) | `std/mem/fn.swap.html` |
| `vec!` (macro) | `std/macro.vec.html` |

Resolution strategy:

1. If query contains `::`, split into segments. The first segment is the crate (`std`, `core`, `alloc`). Remaining segments form the module path + item name.
2. If query is a bare name, look it up in a hard-coded table of common types (`Vec`, `Option`, `Result`, `String`, `Box`, `Rc`, `Arc`, `VecDeque`, `HashMap`, `HashSet`, `BTreeMap`, `BTreeSet`, `Cow`, `Cell`, `RefCell`, `Mutex`, `RwLock`, etc.), plus primitives and well-known traits.
3. If the bare name isn't in the table, try the fallback: walk `sidebar-items*.js` files.
4. If nothing matches, return `None`.

The hard-coded table approach avoids expensive filesystem walks for the most common cases. Less common items fall through to the sidebar search.

#### HTML extraction (V1, zero new deps)

A simple state machine that:

1. Finds the `<main>` or `<section id="main-content">` element
2. Strips all HTML tags (track `<` / `>` depth, emit text outside tags)
3. Decodes HTML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&#39;`, `&#x27;`, numeric entities)
4. Collapses runs of whitespace into single newlines
5. Wraps in `<stdlib reference query="...">...</stdlib reference>`

#### Section filtering

Applied after extraction when the `section` parameter is set:

- `"all"` — return full extracted text
- `"methods"` — find headings containing "Implementations" or "Methods", extract content under those headings until next `h2`
- `"traits"` — find "Trait Implementations" heading, extract that section
- `"examples"` — find all `<pre><code>` blocks
- `"signature"` — extract only the first paragraph (the type declaration)

#### `impl Tool for RustdocTool`

```rust
fn name(&self) -> ToolName { ToolName::from("rustdoc_lookup") }
fn risk(&self) -> ToolRisk { ToolRisk::Read }
fn description(&self) -> &str {
    "Look up Rust standard library documentation from locally installed rustdoc HTML. \
     Pass a type name (Vec), fully-qualified path (std::collections::HashMap), \
     or method query (Option::map). Returns documentation text without network access."
}
fn parameters_schema(&self) -> serde_json::Value {
    // query: required string
    // section: optional string, enum ["all", "methods", "traits", "examples", "signature"]
}
async fn execute(&self, arguments: Value, cancel: CancellationToken) -> Result<ToolOutcome> {
    // 1. Extract query and section from arguments
    // 2. resolve_query(query) -> Option<PathBuf>
    // 3. Read HTML file from doc_root + resolved path
    // 4. Strip HTML, extract text
    // 5. Apply section filter
    // 6. Wrap in <stdlib reference> framing
    // 7. Return ToolResult::success(output)
}
```

#### Unit tests (inline `#[cfg(test)] mod tests`)

- `resolve_vec` — `Vec` resolves to `std/vec/struct.Vec.html`
- `resolve_option` — `Option` resolves to `core/option/enum.Option.html`
- `resolve_hashmap_qualified` — `std::collections::HashMap` resolves correctly
- `resolve_display_trait` — `Display` resolves to `std/fmt/trait.Display.html`
- `resolve_str_primitive` — `str` resolves to `std/primitive.str.html`
- `resolve_unknown_returns_none` — `NonExistentType123` returns `None`
- `html_stripping_removes_tags` — `<p>Hello <em>world</em></p>` → `Hello world`
- `html_entity_decoding` — `&amp; &lt; &gt;` → `& < >`
- `section_filter_methods` — extracts only methods section
- `section_filter_examples` — extracts only code blocks
- `missing_file_returns_error` — graceful handling of nonexistent HTML

---

### 2. `rho-tools/src/rust/mod.rs` — EDIT

Add the new submodule and re-export:

```rust
// Add to internal modules:
mod rustdoc;

// Add to re-exports:
pub use rustdoc::RustdocTool;
```

Update the module doc comment to list `RustdocTool`.

---

### 3. `rho-tools/src/lib.rs` — EDIT

Add `RustdocTool` to the `pub use` line and register it in `register_all()`:

```rust
pub use rust::{CargoCheck, CargoClippy, CargoFix, CargoTest, RustcExplain, RustdocTool};
```

In `register_all()`, add after the other Rust tools:

```rust
registry.register(Box::new(RustdocTool::new()));
```

No `SandboxRoot` or `ShellExecutor` needed — the tool reads from the system rustdoc directory.

---

### 4. `rho-core/src/prompts/base.md` — EDIT

Add under "Working with Rust code":

```markdown
### Looking up standard library documentation

Use `rustdoc_lookup` to verify method signatures, trait implementations, or code examples from the standard library for the installed Rust toolchain. This reads locally installed rustdoc HTML — no network access needed.

- `rustdoc_lookup` — for stdlib API questions (type `Vec`, path `std::sync::Mutex`, method `Option::map`)
- `rustc_explain` — for understanding error codes from compiler output (e.g., `E0308`)
```

---

## Implementation order

| Step | File | What |
|------|------|------|
| 1 | `rho-tools/src/rust/rustdoc.rs` | Struct, `new()`, `resolve_query()`, unit tests for resolution |
| 2 | same file | HTML stripping state machine, entity decoding, unit tests |
| 3 | same file | Section filtering, `<stdlib reference>` framing, unit tests |
| 4 | same file | `impl Tool` with `execute()`, parameters schema |
| 5 | `rho-tools/src/rust/mod.rs` | Add `mod rustdoc` + `pub use RustdocTool` |
| 6 | `rho-tools/src/lib.rs` | Add to `pub use` + register in `register_all()` |
| 7 | `rho-core/src/prompts/base.md` | Add prompt guidance |
| 8 | — | `cargo xtask ci` — verify everything passes |
