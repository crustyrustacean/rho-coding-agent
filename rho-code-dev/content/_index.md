+++
title = "rho"
description = "A coding agent you pair with. Written in Rust, runs on your machine, and asks before it does anything."
+++

<div class="pillar-grid">

<div class="pillar-card">

### [Private](/features/privacy/)

**It asks before it does.**

Writes, edits, and commands wait for you — and you can answer "no, do it
differently," with instructions. Secrets are redacted before anything reaches
the model, and your code never leaves your machine unless you point rho at a
hosted model and tell it to.

</div>

<div class="pillar-card">

### [Stable](/features/stability/)

**Forged in Rust.**

No runtime panics mid-edit, no GC pauses in the tool loop. Every commit runs
format, lint, audit, build, and 1600+ tests before it lands.

</div>

<div class="pillar-card">

### [Flexible](/features/frontends/)

**The frontend is yours.**

Headless by design, speaking JSON-RPC over stdin/stdout. Any UI can drive it —
the official rho-egui desktop app, a terminal client, or something you build.

</div>

<div class="pillar-card">

### [Extendible](/features/extensions/)

**Write tools in TypeScript.**

A single `.ts` file exporting a manifest, running in its own V8 isolate
behind the same approval gate as the built-ins. No separate path, no quiet
escalation.

</div>

</div>
