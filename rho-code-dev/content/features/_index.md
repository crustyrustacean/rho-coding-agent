+++
title = "Features"
description = "Every tool rho ships - file operations, structured search, the Rust toolchain suite, crates.io lookup, and the knowledge base."
+++

rho ships 19 tools. Here's what they do and why the shape matters.

## Files and editing

- **`read_file`** — reads within the project sandbox, and returns content in
  *hashline* format: every line prefixed with a content-derived anchor
  (`9#KT: console.log(...)`). Edits reference those anchors, so a stale read
  fails loudly instead of silently corrupting a file that changed underneath
  the model.
- **`batch_read`** — up to 20 files in one call, same format.
- **`write_file`**, **`list_dir`** — what they say.
- **`edit_file`** — hashline-anchored editing: the model cites the line and
  its hash, and rho verifies the content matches before applying the change.
  Legacy exact-match editing still works as a fallback.

## Finding code

- **`search_files`** — regex content search across the project, returning
  `path:line:` references. Runs on an embedded search engine rather than
  shelling out to `grep`, so it works everywhere and respects ignore files.
- **`find_files`** — name-glob lookup, same properties.

Both exist so the model locates code structurally instead of guessing at
`Get-ChildItem` incantations.

## Shell

- **`run_command`** — PowerShell, through the denylist, inside the sandbox.
- **`wait_for`** — polls a command server-side until it succeeds or times
  out. Waiting for CI costs *one* turn instead of a dozen
  `Start-Sleep`-and-check cycles burning the iteration budget.

## The Rust suite

Six tools that parse structured output instead of handing the model a wall
of text:

- **`cargo_check`**, **`cargo_clippy`**, **`cargo_test`**, **`cargo_fix`** —
  run with NDJSON output; diagnostics come back as error codes, source spans,
  and machine-applicable suggestions, with dependency noise filtered out. The
  model fixes the specific span rather than pattern-matching on prose.
- **`rustc_explain`** — pulls the long-form explanation for an error code.
- **`rustdoc_lookup`** — standard-library docs from the locally installed
  toolchain, so answers match *your* Rust version, not whatever a model
  remembers from training.

## crates.io

**`crates_io_lookup`** — four operations against live crates.io data:
`search` ranked by downloads, `info` (version, license, repo, docs),
`versions` with yanked status, and `deps` for the dependency tree. The model
picks a crate based on current facts rather than recall, and can check
whether a dependency is maintained before adding it.

## Memory

- **`memory`** — a project-local knowledge base (SQLite/FTS5) with six
  operations: store, search, get, update, delete, list. Design decisions,
  conventions, and debugging discoveries persist across sessions, so the
  agent you used last week remembers why, not just what.
- **`session_summary`** — compressed turn history for context recovery
  mid-session.

Everything above runs behind the same approval gate, with the same sandbox
and the same secret redaction. A tool is a capability, not an exemption.
