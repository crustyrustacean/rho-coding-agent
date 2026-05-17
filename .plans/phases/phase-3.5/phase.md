# Phase 3.5: Rust Standard Library Reference ✅ Complete

**Goal:** Give the agent the ability to look up Rust standard library API documentation without network access, using the locally installed rustdoc HTML shipped by `rustup`.

**Milestone:** A new `RustdocLookup` tool resolves type names, method queries, trait names, and fully-qualified paths to the local rustdoc HTML, extracts formatted documentation sections, and returns them to the model.

**Depends on:** Phase 3 | **Effort:** 2–4 days

**Full plan (Obsidian):** [[Phase 3.5 — Rust Standard Library Reference]]

## Motivation

When the model writes Rust code, it relies on its training data for stdlib API knowledge — which may be outdated, incomplete, or wrong for the installed toolchain version. A local rustdoc lookup tool lets the model:

- Verify exact method signatures for the installed Rust version
- Read canonical code examples directly from the docs
- Look up trait implementations for a type
- Find the right stdlib type for a task without guessing

## Design

### Tool: `RustdocLookup`

**Risk level:** `Read` (auto-approved)

**New file:** `rho-tools/src/rustdoc.rs`

#### Parameters
- `query` (required): Item to look up (type names, fully-qualified paths, method queries, trait names)
- `section` (optional, default `"all"`): `all`, `methods`, `traits`, `examples`, `signature`

#### Query Resolution Strategy
1. Direct path mapping — `Vec` → `std/vec/struct.Vec.html`
2. Fully-qualified path — `std::collections::HashMap` → `std/collections/struct.HashMap.html`
3. Method/associated function lookup — `Option::map` → resolve `Option` first, then extract method
4. Trait lookup — `Display` → `std/fmt/trait.Display.html`
5. Fallback: sidebar index search — parse `sidebar-items*.js` files

#### Doc Root Discovery
```
$ rustup doc --path
~/.rustup/toolchains/.../share/doc/rust/html
```
Executed once at tool construction time and cached. Fast-fail if `rustup` not found.

#### HTML Extraction
- **V1 (minimal, zero new deps):** Strip all HTML tags, return plain text of `<main>` content
- **V2 (structured):** Use `scraper` crate for CSS-selector-based extraction (follow-up)

#### Output Framing
Wrapped in `<stdlib reference>` framing (consistent with `<context>` framing pattern).

#### Truncation
Apply the same `ToolResultDetails::FullOutput` truncation pattern used by `CargoCheck`.

## Exit Criteria
- [x] `rustdoc_lookup` tool resolves and returns docs for common stdlib types
- [x] Model can use the tool to answer API questions correctly
- [x] Zero new crate dependencies (V1)
- [x] All existing tests pass
- [x] New tool has ≥ 10 unit tests (37 unit tests)

**Completed:** 2026-05-17

**Eval result:** Scenario 06 passes against Gemma 4 27B (53s, 8 iterations).

**Bonus fixes delivered alongside:**
- `max_tokens` field added to `ChatRequest` — sent as `completion_reserve` (4096) so thinking models get explicit output budget from the server.
- `EntryId` widened from 32-bit to 64-bit to eliminate UUID collision flake in CI.

## Future Work (Beyond Phase 3.5)
- V2: Structured extraction with `scraper`
- V3: In-memory index for instant fuzzy search
- External crate docs (extend to `~/.cargo/registry/src/*/docs/`)
- `--rustdoc-path` CLI flag for overrides
