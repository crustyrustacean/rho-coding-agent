+++
title = "Private by default"
description = "Your code stays on your machine. rho asks before it acts, redacts secrets, and warns before anything leaves."
+++

rho treats its own output as untrusted, and treats your machine as yours.

## It asks before it does

Writes, edits, and shell commands pass through an approval gate configured per
tool:

```toml
[approval.per_tool]
write_file = "ask"
edit_file = "ask"
run_command = "ask"
read_file = "auto"
```

Read-only tools run without interrupting you. Anything that changes state stops
and waits.

You don't have to answer with a plain refusal. Denying with a message — *"no,
use a trait instead"* — feeds that back to the model as a new instruction and it
tries again. That's the difference between a gate and a wall.

## Your code stays yours

rho runs on your machine. There is no cloud round-trip for your source and no
telemetry. Point it at a local model (LM Studio, Ollama) and nothing you write
ever leaves the box.

If you do use a hosted provider, rho tells you plainly the first time — a
consent prompt that says your code is about to be sent elsewhere. It can be
skipped with a flag once you've read it, but it happens once, out loud, not
silently in a config file.

## Defence in depth

- **File sandbox** — every path is validated against the project root before
  anything is read or written.
- **Command denylist** — destructive and network-exfiltration commands are
  refused before a process is ever spawned.
- **Secret redaction** — anything matching a known credential shape is replaced
  with `[REDACTED]` before it reaches the model.
- **Untrusted-data framing** — file contents are wrapped so the model treats
  them as data, not as instructions to follow.
- **Context-file trust** — project instruction files are hash-verified, so a
  changed `AGENTS.md` prompts before it's obeyed.

Each layer can be tightened in `.rho/config.toml`. Some can also be loosened,
which is why it's worth reading the
[security documentation](https://crustyrustacean.github.io/rho-coding-agent)
before you loosen anything.
