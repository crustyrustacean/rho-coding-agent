# Phase 4: Extensions

**Goal:** The agent is extensible. Users can add custom tools, hooks, and commands by writing TypeScript files.

**Milestone:** A user creates a `.ts` file in `~/.rho/extensions/`, restarts the agent, and the model can use the new tool.

**Current state (pre-Phase 4):** The agent loop operates on `Session` (tree-shaped, JSONL-persisted). The `Tool` trait and `ToolRegistry` support `Box<dyn Tool>`. The `AgentObserver` trait provides lifecycle hooks. Streaming is implemented. External providers (OpenRouter) are validated. RPC mode (`--mode rpc`) supports headless JSONL over stdin/stdout. Project context file trust (hash verification, user confirmation, re-confirmation on change) is implemented.

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `deno_core = "0.401.0"` **(foundation)** | `rho-ext` | V8 embedding — provides the JavaScript/TypeScript runtime. Vendored V8 binary adds ~30-50MB to the rho binary. |
| `deno_ast = "0.53.2"` **(foundation)** | `rho-ext` | TypeScript → JavaScript transpilation at load time. No external Deno CLI needed. |

## Decisions

**Extension format:** TypeScript files loaded into V8 isolates via `deno_core`. This matches pi's extension model (write TypeScript, drop it in a directory, it works) and gives extension authors `fetch()`, `async/await`, and the npm ecosystem. See the design document in pi-brain (`c5dcae58`) for the full specification.

**Why not Lua:** Lua is lighter (~1.5MB vs ~50MB) but rho is a long-running service, not an ephemeral CLI. The 50MB overhead is invisible. TypeScript gives extension authors the same language as pi extensions, native `fetch()`, and a type system. Authoring experience matters more than binary size.

**Why not TOML templates:** Phase 5's original plan started with TOML-defined command tools. These cover ~5% of pi's extension power — no logic, no hooks, no state. TypeScript via Deno covers ~95% and is immediately usable by anyone who has written a pi extension.

**Why not WASM:** Adds a compilation step, serialization tax, and async awkwardness. ~3000 lines minimum for the runtime. TypeScript via Deno is the same "write a file, drop it in a directory" experience without the complexity.

**Sandboxing:** Each extension runs in its own V8 isolate with no Deno runtime APIs exposed. Extensions can only do I/O through `rho.*` host functions (or native `fetch()` when granted network permission). The V8 isolate provides real process-level memory isolation, not just library stripping.

**Optional feature flag:** `rho-ext` depends on V8, which adds ~30-50MB to the binary. The `rho` binary enables extensions via a `--features extensions` cargo feature flag. Users who don't need extensions get a lean binary. This is the same pattern used by `rho-highlight` for tree-sitter grammars.

## Threading Model — `JsRuntime` is `!Send`

This is the most important implementation constraint. `JsRuntime` (deno_core 0.401.0) uses `Rc<RefCell<...>>` throughout its internals and is therefore `!Send`. It cannot be placed in an `Arc<tokio::sync::Mutex<...>>` or passed across thread boundaries.

**The correct pattern is a dedicated thread per extension isolate with a channel interface:**

```
DenoTool::execute() / DenoObserver      Extension thread
           │                                    │
           │──── ExtRequest (name, args_json) ──►│  owns JsRuntime
           │◄─── ExtResponse (result_json) ──────│  runs LocalSet
           │                                    │
      (awaits oneshot receiver)         (loops on mpsc receiver)
```

Each loaded extension gets:
- One OS thread (or a `tokio::task::LocalSet` with `spawn_local`) that owns the `JsRuntime` and loops on an `mpsc::Receiver<ExtRequest>`
- A cloneable `mpsc::Sender<ExtRequest>` shared by all `DenoTool` instances and the `DenoObserver` for that extension
- Each request carries a `oneshot::Sender<ExtResponse>` for the reply

This design also eliminates the deadlock that would occur if a tool and an observer hook both tried to access a shared runtime simultaneously.

**Implications for `DenoObserver`:**
- Fire-and-forget hooks (`on_tool_result`, `on_text_delta`) send a message and do not wait for a reply.
- Intercepting hooks (`on_tool_call_intercept`) send a message and block on the `oneshot` receiver. The `AgentObserver` trait methods are synchronous, so this is a brief channel wait (~microseconds for pure-JS checks). This is acceptable.

## `AgentObserver` — Interception Hook

A new method is added to `AgentObserver` in `rho-core`:

```rust
pub enum InterceptDecision {
    Allow,
    Block { reason: String },
}

pub trait AgentObserver: Send + Sync {
    // ... existing methods unchanged ...

    /// Called before a tool executes. Return Block to prevent execution.
    /// Default implementation allows all calls.
    fn on_tool_call_intercept(&self, _name: &str, _args: &str) -> InterceptDecision {
        InterceptDecision::Allow
    }
}
```

The agent loop checks this in `handle_execution` (in `LoopContext`), after approval but before `ToolRegistry::execute`. Non-breaking: `NopObserver` and all existing observers inherit the default `Allow`.

The interception check must happen after approval (so the user has already confirmed) but before execution (so the extension can still block it). The location in `handle_execution` satisfies this.

## `ToolRegistry` — Replace Support for Hot Reload

The current registry panics on duplicate tool names. Hot reload requires replacing existing tools. A `replace` method is added alongside `register`:

```rust
impl ToolRegistry {
    /// Replace an existing tool or register a new one (for hot reload).
    pub fn replace(&mut self, tool: Box<dyn Tool>) { ... }
}
```

`register` retains its panic-on-duplicate behavior for startup safety.

## `rho-ext/Cargo.toml` — Required Dependencies

The current stub is missing several dependencies:

```toml
[dependencies]
deno_ast     = "0.53.2"
deno_core    = "0.401.0"
rho-core     = { path = "../rho-core" }
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio        = { workspace = true, features = ["full"] }
tracing.workspace = true
thiserror.workspace = true
```

## `deno_ast` Transpilation API (0.53.2)

The design document references an older API. The correct call sequence for 0.53.2 is:

```rust
use deno_ast::{EmitOptions, MediaType, ParseParams, SourceTextInfo, TranspileOptions};

let parsed = deno_ast::parse_module(ParseParams {
    specifier: specifier.into(),
    text: SourceTextInfo::from_string(source),
    media_type: MediaType::TypeScript,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
})?;
let transpiled = parsed.transpile(&TranspileOptions::default(), &EmitOptions::default())?;
let js_source = transpiled.into_source().text;
```

## `deno_core` Module Loading API (0.401.0)

The design document's `execute_script::<serde_json::Value>(...)` pattern does not exist. `execute_script` returns a `v8::Global<v8::Value>` and is synchronous-only — it cannot drive async functions. The correct flow for loading a module and calling an async function is:

```rust
// At extension load time:
let mod_id = rt.load_main_es_module_from_code(&specifier, js_source).await?;
let _ = rt.mod_evaluate(mod_id);
rt.run_event_loop(Default::default()).await?;

// Per tool call (on the extension's dedicated thread):
// NOTE: call_with_args_and_await is DEPRECATED in 0.401.0.
// Use call_with_args + with_event_loop_promise instead:
let fn_handle: v8::Global<v8::Function> = /* extract from module namespace */;
let call_future = rt.call_with_args(&fn_handle, &[args_v8]);
let result = rt.with_event_loop_promise(call_future, Default::default()).await?;
// Deserialize result from v8::Global<v8::Value>
```

The `execute_script` method exists but returns a `v8::Global<v8::Value>` and is for synchronous scripts only — not suitable for async `execute(args)` functions.

## Host Function Plumbing

`rho.readFile()`, `rho.writeFile()`, and `rho.runCommand()` need access to `SandboxRoot` and the approval gate. These are captured at extension-load time and stored in `OpState` (deno_core's per-isolate state container):

```rust
// During isolate construction:
let op_state = runtime.op_state();
op_state.borrow_mut().put(sandbox.clone());        // SandboxRoot
op_state.borrow_mut().put(approval_gate.clone());  // Arc<dyn ApprovalGate>
```

Host functions are registered as `deno_core` ops and pull these out of `OpState` when called. This is the standard deno_core pattern for Rust-side state access from JS.

## Exit Criteria

The agent is extensible. Users can define custom tools in TypeScript, the model can use them, hooks can intercept and modify agent behavior, and the system prompt includes extension tool schemas with budget awareness. The REPL and RPC modes are polished production-ready interfaces for daily Rust development.

## Design Reference

Full design document stored in pi-brain: `c5dcae58-2bf9-4cde-a6d7-86ec24f43072` (rho Extension System Design: Deno/TypeScript Extensions).

Key patterns:
- Extension files: `~/.rho/extensions/*.ts` and `.rho/extensions/*.ts` (project-local)
- Multi-file: `*/mod.ts` as entry point, relative imports work
- Extension export: `export default { name, tools: [...], hooks: {...}, commands: [...] }`
- Tool registration: `DenoTool` wraps extension tools as `Box<dyn Tool>` in the existing `ToolRegistry`
- Hook registration: `DenoObserver` wraps extension hooks as `AgentObserver`
- Host functions: `rho.readFile()`, `rho.writeFile()`, `rho.runCommand()`, `rho.log()`, etc.
- Network: `fetch()` is native (granted per-extension via config)
- Type definitions: `rho.d.ts` shipped for extension author IntelliSense
- Hot reload: `/reload` recreates V8 isolates for changed extensions
