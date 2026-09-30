+++
title = "rho"
description = "A coding agent you pair with. Written in Rust, runs on your machine, and asks before it does anything."
+++

## Get it running

```sh
git clone https://github.com/crustyrustacean/rho-coding-agent
cd rho-coding-agent
cargo xtask ci          # fmt, lint, audit, build, test — the lot
```

You need a Rust toolchain, [PowerShell 7+](https://learn.microsoft.com/en-us/powershell/scripting/install/installing-powershell),
and a model. rho talks to a local server (LM Studio, Ollama) or any hosted
provider, so you choose which one.

Then get the UI:

```sh
git clone https://github.com/crustyrustacean/rho-egui && cd rho-egui
cargo run
```

[rho-egui](https://github.com/crustyrustacean/rho-egui) is the official
desktop UI. It spawns `rho` in the background and talks to it over JSON-RPC.

<section class="feature-section">

## **Private — it asks before it does**

Your code doesn't leave your machine unless you point rho at a hosted model,
and you get a clear warning the first time you do. Every write, edit, and
command passes an approval gate — and you can answer "no, do it differently"
with instructions, not just a refusal. Secrets are redacted before anything
reaches the model.

</section>

<section class="feature-section">

## **Stable — forged in Rust**

No runtime panics swallowing your work mid-edit, no GC pauses in a tool loop.
The same code that runs the agent runs its 1600+ tests, and the workspace
gates every commit on format, lint, audit, build, and test before it lands.
`cargo check` and `cargo clippy` come back as structured diagnostics — error
codes and source spans — so the model fixes the actual problem rather than
pattern-matching on a wall of compiler output.

</section>

<section class="feature-section">

## **Flexible — the frontend is yours**

rho is headless by design. It speaks JSON-RPC 2.0 over stdin/stdout, so any
UI can drive it: the official [rho-egui](https://github.com/crustyrustacean/rho-egui)
desktop app, a terminal client, or something you build. Swap frontends
without changing anything about how the agent works — and swap models the
same way, local or hosted, per project.

</section>

<section class="feature-section">

## **Extendible — write tools in TypeScript**

Need a tool rho doesn't ship? Write it in TypeScript: a single `.ts` file
exporting a manifest, running in its own V8 isolate with a host API for
files, commands, and logging. `risk` on each tool feeds the same approval
gate as the built-ins, so an extension can't quietly escalate. `/reload`
picks up changes without a restart.

</section>

## Read more

Full documentation lives at
[crustyrustacean.github.io/rho-coding-agent](https://crustyrustacean.github.io/rho-coding-agent),
and the source is on [GitHub](https://github.com/crustyrustacean/rho-coding-agent).
