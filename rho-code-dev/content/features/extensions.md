+++
title = "Extendible in TypeScript"
description = "Add tools, hooks, and commands as a single .ts file - each extension in its own V8 isolate, behind the same approval gate."
+++

Need a tool rho doesn't ship? Write it in the language you already know.

## One file, one extension

```ts
export default {
  name: "hello",
  tools: [{
    name: "hello",
    description: "Greet someone",
    risk: "read" as const,
    parameters: {
      name: { type: "string", description: "Who to greet", required: true },
    },
    execute: async (args: string) => {
      return JSON.stringify({ output: `Hello, ${args}!` });
    },
  }],
};
```

Drop it in `~/.rho/extensions/` (yours) or `.rho/extensions/` (per-project) and
it's live. `/reload` picks up changes without a restart.

## Isolated by construction

Each extension runs in its own V8 isolate, with a host API for the things an
agent legitimately needs — reading and writing sandboxed files, running
commands, logging, fetching. It cannot reach past the sandbox or touch the
agent's own memory.

## Same rules as the built-ins

Every tool an extension declares carries a `risk`, and that risk feeds the same
approval gate as `run_command` or `write_file`. An extension cannot quietly
escalate past what you've approved, because there's no separate path for it to
take.

TypeScript definitions ship with the crate, so your editor autocompletes the
manifest shape.
