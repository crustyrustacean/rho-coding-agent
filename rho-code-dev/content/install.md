+++
title = "Install"
description = "Get rho and the rho-egui desktop UI running on your machine."
+++

## What you need

- **A Rust toolchain** — edition 2024, so a current stable.
- **PowerShell 7+** (`pwsh`) — rho runs shell commands through it. This is
  deliberate: the model generates PowerShell, not bash, so what you see is what
  runs.
- **A model** — either a local server ([LM Studio](https://lmstudio.ai/),
  [Ollama](https://ollama.com/)) or an API key for a hosted provider. You choose;
  rho has no opinion.

## Get rho

```sh
git clone https://github.com/crustyrustacean/rho-coding-agent
cd rho-coding-agent
cargo build --release --package rho
```

Put the binary somewhere on your `PATH`. `target/release/rho.exe` is where it
lands on Windows.

## Get the UI

[rho-egui](https://github.com/crustyrustacean/rho-egui) is the official desktop
UI — a native app that spawns `rho` in the background and renders the agent as a
chat.

```sh
git clone https://github.com/crustyrustacean/rho-egui && cd rho-egui
cargo run
```

It looks for `rho` on your `PATH`. If you'd rather point it at a specific build,
set `RHO_PATH`:

```sh
RHO_PATH=/path/to/rho cargo run
```

## Point it at a model

**Local model** — start LM Studio or Ollama on `localhost:1234`, then:

```sh
rho --model <model-id>
```

**Hosted provider** — put your key in an environment variable and add a
provider to `.rho/config.toml`. Keys are never stored in config:

```toml
[agent]
model = "gpt-4o"
token_budget = 131072

[[providers]]
preset = "openai"
api_key_env = "OPENAI_API_KEY"
```

```sh
export OPENAI_API_KEY="sk-..."
rho --accept-external-provider
```

That flag skips the one-time consent prompt telling you your code is about to
leave your machine. You should read it the first time.

## If something goes wrong

rho keeps two records worth knowing about. Session data — the full entry
history for every project — lives under `~/.rho/sessions`, filed by project.
And `logs/` inside the project holds the runtime logs for the work you did
there. When something misbehaves, the session file tells you *what the agent
saw*; the log tells you *what the agent did*.

The [documentation](https://crustyrustacean.github.io/rho-coding-agent) covers
providers, extensions, and configuration in full.
