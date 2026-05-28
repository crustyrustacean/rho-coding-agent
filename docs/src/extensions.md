# Extensions

rho supports TypeScript extensions that add custom tools, hooks, and commands without modifying the core codebase. Extensions run in isolated V8 isolates via `rho-ext` (powered by `deno_core`).

## Overview

Extensions are TypeScript files placed in extension directories:

```
~/.rho/extensions/           # User-level (available in all projects)
  ├── hello.ts               # Single-file extension
  └── rust_docs/
      ├── mod.ts             # Entry point for multi-file extension
      ├── parser.ts          # Helper module
      └── cache.ts           # In-memory cache

.rho/extensions/             # Project-level (overrides user-level by name)
  └── project_tool.ts
```

Each extension gets its own V8 isolate on a dedicated OS thread. The extension's `export default { ... }` manifest declares tools, hooks, and commands. TypeScript is transpiled to JavaScript at load time via `deno_ast` — no external Deno CLI or build step needed.

## Extension format

```typescript
/// <reference path="~/.rho/types/rho.d.ts" />

export default {
  name: "my-extension",
  version: "1.0.0",

  tools: [{
    name: "search",
    description: "Search documentation",
    risk: "read" as const,
    parameters: {
      query: { type: "string", description: "Search query", required: true },
    },
    async execute(args: { query: string }) {
      const resp = await fetch(`https://docs.example.com/search?q=${encodeURIComponent(args.query)}`);
      if (!resp.ok) return { error: `HTTP ${resp.status}` };
      const text = await resp.text();
      return { output: text };
    },
  }],

  hooks: {
    async onLoad() {
      rho.log("info", "my-extension loaded");
    },
    async onToolCall(toolName: string, args: Record<string, unknown>) {
      // Return { block: true, reason: "..." } to deny
      rho.log("debug", `tool called: ${toolName}`);
    },
    async onToolResult(toolName: string, result: ToolResult) {
      // Can modify the result
    },
    async onBeforeModel(messages: unknown[]) {
      return { inject: `Current time: ${new Date().toISOString()}` };
    },
  },

  commands: [{
    name: "deploy",
    description: "Deploy to production",
    async handler(args: string) {
      rho.log("info", `Deploying: ${args || "default"}`);
    },
  }],
};
```

## Available APIs

### Built-in (from V8)

- **Web APIs:** `fetch()`, `URL`, `URLSearchParams`, `TextEncoder`/`TextDecoder`, `setTimeout`, `setInterval`, `console.log`
- **JavaScript built-ins:** `Promise`, `Array`, `Map`, `Set`, `RegExp`, `JSON`, `Date`, `crypto.subtle`

### rho host functions (the `rho` global)

```typescript
declare namespace rho {
  function log(level: "trace" | "debug" | "info" | "warn" | "error", message: string): void;
  function readFile(path: string): Promise<string>;
  function writeFile(path: string, content: string): Promise<boolean>;
  function runCommand(command: string, args?: string[]): Promise<{
    stdout: string;
    stderr: string;
    exitCode: number;
  }>;
  function getCwd(): string;
  function getModel(): string;
}
```

### Not available (by design)

- No `Deno.*` namespace
- No `Node.js` APIs (`require`, `node:fs`, `child_process`)
- No arbitrary filesystem access (must go through `rho.readFile()`/`rho.writeFile()`)
- No npm packages (extensions are self-contained)

## Configuration

Extensions are configured in `.rho/config.toml` or `~/.rho/config.toml`:

```toml
[extensions]
# Allowlist: only load these extensions (omit to load all)
enabled = ["hello", "crates-search"]

[extensions.defaults]
# Default permissions for all extensions
network = true           # allow fetch()
max_memory_mb = 64       # V8 heap limit per isolate
max_execution_time_s = 30

[extensions.per_extension."rust-docs"]
# Override defaults for a specific extension
max_memory_mb = 128      # needs more for HTML parsing

[extensions.per_extension."dangerous-tool"]
network = false          # deny fetch()
commands = false         # deny rho.runCommand()
```

## Hot reload

Use the `/reload` REPL command to hot-reload extensions without restarting rho:

- Extensions with changed file mtimes are destroyed and re-spawned
- New extensions are loaded; removed extensions are shut down
- Tool registry is updated (stale tools unregistered, new tools registered)
- `onLoad` hooks fire on new/reloaded extensions

V8 isolate creation takes ~20–50ms per extension. For 5 extensions, full reload is ~100–250ms.

## REPL commands

| Command | Description |
|---|---|
| `/reload` | Hot-reload all extensions from disk |
| `/extensions` | List names of currently loaded extensions |

## Extension types

Type definitions ship at `rho-ext/types/rho.d.ts`. Extension authors can reference them for IntelliSense:

```typescript
/// <reference path="~/.rho/types/rho.d.ts" />
```

## Architecture

The extension system is implemented in the `rho-ext` crate:

| Component | Purpose |
|---|---|
| `ExtensionRuntime` | Owns a V8 isolate on a dedicated OS thread, handles async calls |
| `ExtensionLoader` | Discovery, filtering, spawning, tool registration, hot reload |
| `DenoTool` | Wraps extension tool functions as `Box<dyn Tool>` for `ToolRegistry` |
| `DenoObserver` | Wraps extension hooks as `AgentObserver` for the agent loop |
| `CompositeObserver` | Fans out agent-loop events to REPL/RPC + extension observers |
| `RhoModuleLoader` | ESM module resolution for multi-file extensions |
| `transpile.rs` | TS → JS transpilation via `deno_ast` |

### Tool-call interception

Extensions can block tool calls via the `onToolCall` hook. The `InterceptResult` enum (`Block`/`Allow`) is checked in `classify_call()` before the approval policy and execution. If any extension returns `Block`, the call is denied and an error result is fed to the model.

### Model propagation

When the model is switched via `/model` or RPC `set_model`, the new model identifier is propagated to all loaded extensions so `rho.getModel()` returns the current model.

## Performance

| Resource | Per extension |
|---|---|
| Memory | ~10–20 MB (V8 heap, configurable) |
| Startup | ~20–50 ms (isolate creation + transpilation) |
| Thread | 1 dedicated OS thread |

For rho's deployment model (long-running service, 3–8 extensions typical), this overhead is negligible. Extension bridge overhead per tool call is ~100µs–1ms, imperceptible compared to LLM round-trip times.

## Examples

### Error code lookup

```typescript
export default {
  name: "rust-error-codes",
  tools: [{
    name: "error_code",
    description: "Look up a Rust compiler error code (e.g. E0277)",
    risk: "read" as const,
    parameters: {
      code: { type: "string", description: "Error code (e.g. E0277)", required: true },
    },
    async execute(args: { code: string }) {
      const code = args.code.toUpperCase();
      if (!/^E\d+$/.test(code)) {
        return { error: "Invalid format. Use E followed by digits." };
      }
      const resp = await fetch(`https://doc.rust-lang.org/stable/error_codes/${code}.html`);
      if (!resp.ok) return { error: `${code} not found` };
      const html = await resp.text();
      const text = html.replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim();
      return { output: text };
    },
  }],
};
```

### Approval gate

```typescript
export default {
  name: "no-force",
  hooks: {
    async onToolCall(toolName: string, args: Record<string, unknown>) {
      if (toolName !== "run_command") return;
      const cmd = (args.command as string) ?? "";
      if (cmd.includes("-Force") || cmd.includes("--force")) {
        return { block: true, reason: "Force flags blocked by no-force extension" };
      }
    },
  },
};
```

## Session extension entries

rho's session tree supports typed extension entries that are versioned and schema-skew-safe:

```rust
// Write typed state (never sent to the model)
session.write_custom_state("rho.diagnostics.v1", &json!({ "errors": 3 }))?;

// Read back
if let Some(state) = session.read_custom_state("rho.diagnostics.v1")? {
    println!("errors: {}", state["errors"]);
}
```

Extension entry kinds use the format `<author>.<feature>.v<n>`. When rho encounters an unknown kind version, it returns `None` (graceful degradation).

## Future work

| Item | Priority | Notes |
|---|---|---|
| Path utility host functions | Low | `rho.joinPath`, `rho.basename`, etc. |
| `rho.truncate()` host function | Low | Same limits as built-in tools |
| `rho ext init` scaffolding | Medium | Create extension directory structure |
| Extension manifest versioning | Low | Schema evolution |
| Extension marketplace / sharing | Future | Community extension distribution |
