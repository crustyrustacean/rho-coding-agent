# rho-memory Integration Plan

## Goal
Wire `rho-memory` into the agent as project-local knowledge base tools, enabling the LLM to store and recall knowledge across sessions.

## Database Location
Project-local: `<project-root>/.rho/memory.db`
- Inside the existing `.rho/` directory used for project config and extensions
- Auto-created on first tool use (like sessions)

## Integration Approach
Surface 1: Agent tools in `rho-tools`, no changes to `rho-core` or `rho-ai`.

## Dependency Graph (after)
```
rho → rho-tools → rho-memory  (new edge)
                → rho-highlight → rho-core → rho-ai
```

---

## Phase 1: Fix rho-memory CI issues
Must pass `cargo xtask ci` before any integration work.

- [ ] Fix `cargo fmt` — all files have style drift
- [ ] Fix clippy errors (~43):
  - 4× `empty_line_after_doc_comments` — change `///` to `//!` for module-level docs in `brain.rs`, `db.rs`, `error.rs`, `models.rs`
  - 1× `similar_names` — rename `context` to `window` in `extract_excerpt`
  - 5× `doc_markdown` — backtick `SQLite` in doc comments
  - ~15× `missing_docs_in_private_items` — add doc comments to private modules, fields, methods
  - ~11× `missing_errors_doc` — add `# Errors` sections to all `Result`-returning public methods
  - 2× `redundant_closure` — `metadata.map(serde_json::to_string)`
  - 1× `manual_let_else` — use `let...else` in `update_document`
  - 4× `uninlined_format_args` — use `{var}` syntax
  - 2× `cast_possible_wrap` — use `i64::try_from(limit)?`
  - 1× `format_collect` — rewrite hex encoder with `fold`/`write!`
- [ ] Fix UTF-8 panic risk in `extract_excerpt` — use `char_indices` or `.is_char_boundary()` for safe slicing
- [ ] Verify `cargo xtask ci -p rho-memory` passes

## Phase 2: Memory tool implementation
Create the tool that exposes memory to the LLM.

- [ ] Add `rho-memory` as a dependency of `rho-tools/Cargo.toml`
- [ ] Create `rho-tools/src/memory.rs` with `MemoryTool` implementing the `Tool` trait
  - Single tool with `operation` parameter (like `CratesIoLookup` pattern):
    - `store` — create document (title, content, tags)
    - `search` — full-text search (query, tags, limit)
    - `get` — fetch by id
    - `update` — partial update (id + optional fields)
    - `delete` — soft delete by id
    - `list` — paginated list
  - `ToolRisk::Write` for store/update/delete, `ToolRisk::Read` for search/get/list
- [ ] Add `rho-tools/src/memory.rs` module declaration to `lib.rs`
- [ ] Export `MemoryTool` from `rho-tools/src/lib.rs`
- [ ] Modify `register_all()` in `rho-tools/src/lib.rs`:
  - Accept an `Option<Arc<Memory>>` parameter (None when memory is disabled)
  - If Some, register `MemoryTool`
  - Add memory DB path to `SessionPathHolder` or a new `MemoryPathHolder` pattern

## Phase 3: Config and startup wiring

- [ ] Add `[memory]` config section to `rho-core/src/config.rs`:
  ```toml
  [memory]
  enabled = true  # default: false
  ```
- [ ] Update `rho/src/app.rs` startup:
  - After sandbox resolution, check `config.memory.enabled`
  - If enabled, open `Memory` at `<sandbox-root>/.rho/memory.db`
  - Pass `Arc<Memory>` through to `register_all()`

## Phase 4: System prompt

- [ ] Add memory usage instructions to `rho-core/src/prompts.rs` (or `compose_full_system_prompt`):
  - When to store: after completing significant design decisions, architectural patterns, project conventions, debugging discoveries
  - When to search: at the start of a new task, before making architectural decisions
  - Format guidance: concise, factual, tagged with relevant categories

## Phase 5: Testing

- [ ] Unit tests for `MemoryTool` execute method (each operation)
- [ ] Verify tool schema, risk levels, name
- [ ] Integration test: agent uses memory tool across turns

## Phase 6: Documentation and CI

- [ ] Update `ARCHITECTURE.md` — add `rho-memory` to dependency graph, crate responsibilities, project layout, key types
- [ ] Run full `cargo xtask ci` — everything must pass
