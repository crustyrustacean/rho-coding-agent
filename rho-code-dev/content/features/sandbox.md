+++
title = "Sandboxed by default"
description = "File operations are confined to the project root, secrets are redacted, and dangerous commands are blocked."
+++

An agent that reads and writes files on your machine should be paranoid about
it. rho's defences are layered:

- **File sandbox** — every path is validated against the project root before
  anything is read or written.
- **Command denylist** — destructive and network-exfiltration commands are
  refused before a process is ever spawned.
- **Secret redaction** — anything matching a known credential shape is replaced
  with `[REDACTED]` before it reaches the model.
- **Untrusted-data framing** — file contents are wrapped so the model treats them
  as data, not as instructions to follow.
- **Context-file trust** — project instruction files are hash-verified, so a
  changed `AGENTS.md` prompts before it's obeyed.

Each layer can be tightened in `.rho/config.toml`. Some of them can also be
loosened, which is why it's worth reading the
[security documentation](https://crustyrustacean.github.io/rho-coding-agent)
before you loosen anything.
