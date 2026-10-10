+++
title = "Forged in Rust"
description = "No runtime panics mid-edit, no GC pauses in the tool loop, and 1600+ tests gating every commit."
+++

The agent you trust with your working tree shouldn't crash taking your files
with it.

## Why the language matters

rho is a single Rust workspace with no runtime between it and the metal. That
buys three things you can feel in daily use:

- **No surprise termination.** No runtime exception that kills the process with
  your edits unsaved. Errors are values, handled where they occur.
- **No pauses.** There's no garbage collector stopping the world mid-tool-loop,
  so a long session behaves the same at hour three as at minute one.
- **Predictable resource use.** Streaming is bounded, memory is released when a
  session closes, and an idle rho costs effectively nothing.

## Structured diagnostics

Most agents shell out to `cargo check` and hand the model the raw output. rho
parses the NDJSON instead: each diagnostic carries its error code, message,
source spans, and machine-applicable suggestions. Dependency noise is filtered
out, so the model sees *your* code's problems rather than a thousand lines
about someone else's crate.

The practical difference: the model points at a specific span and makes a
targeted edit, instead of guessing from prose. A fix that doesn't compile
produces a real error it can read, not a shrug.

## Tested like it matters

Every commit to rho runs the same gate: format, lint, audit, build, and the
full test suite — 1600+ tests across the workspace. Branch protection enforces
it on trunk. Nothing lands because someone was confident; it lands because the
suite passed.
