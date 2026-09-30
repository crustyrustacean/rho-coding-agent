+++
title = "Extensions"
description = "Add tools, hooks, and commands in TypeScript — each extension runs in its own V8 isolate."
+++

rho has a TypeScript extension system. An extension is a single `.ts` file that
exports a manifest:

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

Each extension gets its own V8 isolate, with a host API for the things an agent
legitimately needs — reading and writing sandboxed files, running commands, and
logging. `risk` on each tool feeds the same approval gate as the built-ins, so
an extension can't quietly escalate.

Live in `~/.rho/extensions/` (yours) or `.rho/extensions/` (per-project).
`/reload` picks up changes without a restart.

TypeScript definitions ship with the crate, so your editor will autocomplete the
manifest shape.
