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

## **It asks before it acts**

Every write, edit, and command goes through an approval gate. rho will not
touch a file or run a shell command without you saying yes, and you can answer
"no, do it differently" with instructions instead of just refusing.

</section>

<section class="feature-section">

## **It knows what your code is doing**

`cargo check` and `cargo clippy` come back as structured diagnostics, not raw
text. rho reads the error codes and the source spans, so it can fix the actual
problem rather than pattern-matching on a wall of compiler output.

</section>

<section class="feature-section">

## **Your code stays yours**

rho runs on your machine. There's no cloud round-trip for your source, no
telemetry, and a file sandbox that confines every operation to the project
root. What the model sees, it sees because you chose to point it there.

</section>

<section class="feature-section">

## **Sessions you can go back to**

Work is a tree, not a log. Fork a branch, try a different approach, and switch
back — the transcript and the context window follow you. Branches survive a
restart.

</section>

## Read more

Full documentation lives at
[crustyrustacean.github.io/rho-coding-agent](https://crustyrustacean.github.io/rho-coding-agent),
and the source is on [GitHub](https://github.com/crustyrustacean/rho-coding-agent).
