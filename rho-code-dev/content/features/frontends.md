+++
title = "Frontend-agnostic"
description = "rho is headless and speaks JSON-RPC 2.0. Any UI can drive it, and the official one is rho-egui."
+++

rho has no opinion about what you look at.

## Headless by design

The agent is a process speaking JSON-RPC 2.0 over stdin/stdout. It emits
streaming notifications — message deltas, tool calls, approvals, usage — and
takes prompts, approvals, and steering back. That's the whole interface.

A clean wire protocol means the UI is a choice, not a commitment:

- **[rho-egui](https://github.com/crustyrustacean/rho-egui)** — the official
  desktop UI. Native, with approvals, session and branch management, and live
  usage tracking.
- **Your own client** — the protocol is documented
  ([rpc-mode](https://crustyrustacean.github.io/rho-coding-agent/rpc-mode.html),
  [OpenRPC schema](https://github.com/crustyrustacean/rho-coding-agent/blob/trunk/docs/rpc-schema/openrpc.json))
  and stable enough to build against.

## Models are a choice too

The same philosophy applies to the model. rho talks to a local server — LM
Studio, Ollama — or any hosted provider, configured per project:

```toml
[agent]
model = "qwen3-8b"

[[providers]]
preset = "lm-studio"
```

Swap the model the way you'd swap the frontend: by changing a line of config,
not by adopting a different tool.

## The payoff

Because the agent doesn't know what's watching it, nothing about the agent
changes when the front end does. A better UI doesn't fork the project, and a
UI experiment can't break the agent underneath.
