+++
title = "Structured diagnostics"
description = "cargo check and clippy come back as error codes and source spans, not a wall of text."
+++

Most agents shell out to `cargo check` and hand the raw output to the model.
rho parses the NDJSON instead.

Each diagnostic carries its error code, message, source spans, and
machine-applicable suggestions. Dependency noise is filtered out, so what the
model sees is *your* code's problems rather than a thousand lines about someone
else's crate.

The practical difference: the model can point at a specific span and make a
targeted edit, instead of guessing from prose. It also means a fix that doesn't
compile produces a real error it can read, not a shrug.
