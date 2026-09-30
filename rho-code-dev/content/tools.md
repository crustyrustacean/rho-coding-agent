+++
title = "Tools"
description = "Every tool rho ships - file operations, structured search, the Rust toolchain suite, crates.io lookup, and the knowledge base."
+++

19 tools, one gate: everything below runs behind the same approval gate,
sandbox, and secret redaction. A tool is a capability, not an exemption.

<div class="card-grid">

<div class="tool-card">

### Files & editing

**`read_file`** — sandboxed reads in *hashline* format: every line carries a
content-derived anchor. Edits cite anchors, so a stale read fails loudly
instead of corrupting a file that changed underneath the model.

**`batch_read`** — up to 20 files, same format.

**`write_file`** / **`list_dir`** — what they say.

**`edit_file`** — hashline-anchored editing; rho verifies the content matches
before applying. Legacy exact-match as a fallback.

</div>

<div class="tool-card">

### Finding code

**`search_files`** — regex content search returning `path:line:` references,
on an embedded engine rather than shelling out to `grep`. Ignores what your
ignore files ignore.

**`find_files`** — name-glob lookup, same properties.

Both exist so the model locates code structurally instead of guessing at
`Get-ChildItem` incantations.

</div>

<div class="tool-card">

### Shell

**`run_command`** — PowerShell, through the denylist, inside the sandbox.

**`wait_for`** — polls a command server-side until it succeeds or times out.
Waiting for CI costs *one* turn instead of a dozen
`Start-Sleep`-and-check cycles burning the iteration budget.

</div>

<div class="tool-card">

### Rust: check, fix, test

**`cargo_check`** / **`cargo_clippy`** / **`cargo_test`** / **`cargo_fix`** —
run with NDJSON output. Diagnostics come back as error codes, source spans,
and machine-applicable suggestions, dependency noise filtered. The model
fixes the specific span rather than pattern-matching on prose.

</div>

<div class="tool-card">

### Rust: docs & errors

**`rustc_explain`** — the long-form explanation for any error code.

**`rustdoc_lookup`** — standard-library docs from your locally installed
toolchain, so answers match *your* Rust version, not training-data recall.

</div>

<div class="tool-card">

### crates.io

**`crates_io_lookup`** — live crates.io data, four operations: `search`
ranked by downloads, `info` (version, license, repo, docs), `versions` with
yanked status, `deps` for the dependency tree. The model picks crates from
current facts and can check whether one is maintained before adding it.

</div>

<div class="tool-card">

### Memory

**`memory`** — a project-local knowledge base (SQLite/FTS5): store, search,
get, update, delete, list. Design decisions and debugging discoveries persist
across sessions, so the agent remembers *why*, not just *what*.

**`session_summary`** — compressed turn history for mid-session context
recovery.

</div>

</div>
