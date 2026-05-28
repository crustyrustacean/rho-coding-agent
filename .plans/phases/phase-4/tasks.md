# Phase 4 Tasks

## Prerequisite spike (do this first)

**0. Architecture spike — validate the threading model and deno_core 0.401.0 API.**

   Before writing any production code, build a minimal proof-of-concept in `rho-ext/src/spike.rs`
   (or a standalone example) that:
   - Spawns a dedicated OS thread owning a `JsRuntime`
   - Transpiles a trivial TypeScript snippet (`async function greet(name: string): Promise<string>`)
     using `deno_ast 0.53.2`
   - Loads the resulting JavaScript as an ES module via `rt.load_main_es_module_from_code`
   - Calls the async function via `rt.call_with_args_and_await` and drives the event loop
   - Returns the result to the calling thread over a `oneshot` channel

   This validates the entire vertical slice (transpile → load → call → receive) with the real 0.401.0
   API before any architecture is committed. Expected size: ~150-200 lines. Delete or keep as a
   documented example after the spike passes.

   **Why first:** `JsRuntime` is `!Send` (uses `Rc<RefCell<...>>` internally). The
   `Arc<tokio::sync::Mutex<JsRuntime>>` pattern in the pi-brain design doc will not compile against
   the real crate. This spike confirms the correct threading pattern and uncovers any other API
   surprises before they affect the rest of the implementation.

---

## Phase 4 tasks (ordered)

1. **Complete `rho-ext/Cargo.toml`.**
   - Add the missing dependencies to the existing stub:
     `rho-core`, `async-trait`, `serde`, `serde_json`, `tokio` (features = ["full"]),
     `tracing`, `thiserror`
   - The current file only has `deno_ast` and `deno_core`

2. **Module structure.**
   - Establish the internal layout: `lib.rs`, `discovery.rs`, `loader.rs`, `runtime.rs`,
     `deno_tool.rs`, `deno_observer.rs`, `host_functions.rs`, `types.rs`
   - `runtime.rs` owns the `ExtensionRuntime` struct (the dedicated thread + channel pair)

3. **TypeScript transpilation pipeline (`deno_ast 0.53.2`).**
   - Use the verified API from the spike:
     `parse_module(ParseParams { ... media_type: MediaType::TypeScript ... })`
     then `parsed.transpile(&TranspileOptions::default(), &EmitOptions::default())`
   - Cache transpiled output keyed by `(path, mtime)` for faster reloads
   - Return a `TranspileError` (wrapped in `rho-ext`'s error type) on parse/transpile failure

4. **`ExtensionRuntime` — dedicated thread per isolate (`deno_core 0.401.0`).**
   - Each loaded extension gets one OS thread running a `tokio::task::LocalSet`
   - The thread owns the `JsRuntime` and loops on an `mpsc::Receiver<ExtRequest>`
   - `ExtRequest` carries: `(call_id, fn_name, args_json, oneshot::Sender<ExtResponse>)`
   - The thread calls `rt.call_with_args_and_await(fn_handle, &[args_v8]).await`,
     drives `rt.run_event_loop`, and sends the result back on the `oneshot` sender
   - `ExtensionRuntime` is the public handle: cloneable `mpsc::Sender<ExtRequest>` + thread `JoinHandle`
   - Shutdown: drop the sender (thread exits its loop), join the handle

5. **V8 isolate sandboxing.**
   - Load only the ops/extensions rho explicitly enables — no `deno_runtime` crate, no `Deno.*`
     namespace, no `node:*` module loader
   - Inject `rho.*` host functions as `deno_core` ops (see Task 8)
   - Set V8 heap limit via `JsRuntime::add_near_heap_limit_callback` (default: 64 MB, configurable)
   - Set execution timeout: the extension thread's event loop is raced against a `tokio::time::sleep`
     on the calling side; if the `oneshot` receiver times out, the caller returns
     `ToolResult::error("extension timeout")` and logs a warning

6. **Extension discovery and loading.**
   - Scan `~/.rho/extensions/*.ts`, `~/.rho/extensions/*/mod.ts` (global)
   - Scan `.rho/extensions/*.ts`, `.rho/extensions/*/mod.ts` (project-local, relative to sandbox root)
   - For each file: transpile (Task 3) → spawn `ExtensionRuntime` thread (Task 4) →
     execute the module → extract and validate the default export
   - Default export validation: check required fields (`name`, `tools`), report missing/wrong-type
     fields as `ExtensionLoadError::InvalidManifest` with a clear message
   - Return `Vec<LoadedExtension>` where each entry holds the `ExtensionRuntime` sender,
     parsed tool definitions, parsed hook presence flags, and parsed command definitions

7. **`DenoTool` — implement the `Tool` trait.**
   - Holds: `ToolName`, `description`, `parameters_schema`, `ToolRisk`,
     and a clone of the `ExtensionRuntime` sender
   - `execute(&self, arguments, cancel)`:
     - Serialize `arguments` to JSON string
     - Send `ExtRequest { fn_name: "execute", args_json, reply: oneshot_tx }` to the extension thread
     - Race `oneshot_rx` against `cancel.cancelled()` and the configured timeout
     - Deserialize the `ExtResponse` into `ToolResult` (check for `error` field)
   - Register as `Box<dyn Tool>` in `ToolRegistry` via the existing `register` path

8. **Host functions (`rho.*` ops).**
   - Register as `deno_core` ops, capturing context via `OpState`:
     - `OpState` holds `SandboxRoot`, `Arc<dyn ApprovalGate>`, and model/session info
     - Injected at `ExtensionRuntime` construction time
   - Ops to implement:
     - `rho.readFile(path)` — validates against `SandboxRoot`, reads file, returns string
     - `rho.writeFile(path, content)` — validates sandbox, writes file
     - `rho.runCommand(command, args?)` — goes through `ApprovalGate` (blocks extension thread
       briefly while waiting for approval); subject to `CommandDenylist`
     - `rho.log(level, message)` — calls `tracing::{debug,info,warn,error}!`
     - `rho.getCwd()`, `rho.getModel()` — read from session info in `OpState`
     - `rho.pathJoin()`, `rho.pathBasename()`, `rho.pathDirname()` — thin wrappers over `std::path`
     - `rho.truncate(text, maxBytes?, maxLines?)` — same truncation logic as built-in tools
   - Network (`fetch()`): available natively in V8 when extension config sets `network = true`;
     disabled by not loading the fetch op when `network = false`

9. **`DenoObserver` — implement `AgentObserver`.**
   - Holds: `ExtensionRuntime` sender, presence flags for each hook
   - Fire-and-forget hooks (no reply needed):
     - `on_tool_result`, `on_text_delta`, `on_reasoning_delta` — send message, don't wait
   - Intercepting hook (`on_tool_call_intercept`):
     - Send message, **block** on `oneshot` receiver with a short timeout (default: 500ms)
     - Deserialize reply: `{ block: true, reason: "..." }` → `InterceptDecision::Block { reason }`
     - Timeout or error → `InterceptDecision::Allow` with a logged warning (fail-open)

10. **`AgentObserver` interception hook — change in `rho-core`.**
    - Add `InterceptDecision` enum and `on_tool_call_intercept` method to `AgentObserver`
      in `rho-core/src/agent.rs`:
      ```rust
      pub enum InterceptDecision { Allow, Block { reason: String } }

      pub trait AgentObserver: Send + Sync {
          // ... existing methods ...
          fn on_tool_call_intercept(&self, _name: &str, _args: &str) -> InterceptDecision {
              InterceptDecision::Allow
          }
      }
      ```
    - Non-breaking: `NopObserver` and all existing observers inherit the default `Allow`
    - Add the check in `LoopContext::handle_execution`, after the approval gate but before
      `ToolRegistry::execute`:
      ```rust
      if let InterceptDecision::Block { reason } = params.observer.on_tool_call_intercept(...) {
          // Append error result, advance to next call
      }
      ```

11. **`ToolRegistry::replace` for hot reload — change in `rho-core`.**
    - Add `pub fn replace(&mut self, tool: Box<dyn Tool>)` to `ToolRegistry`
    - Replaces an existing tool with the same name, or inserts if not found
    - `register` retains its panic-on-duplicate behavior (startup safety)

12. **Config integration.**
    - Add `ExtensionsConfig` to `RhoConfig` in `rho-core/src/config.rs`:
      ```toml
      [extensions]
      enabled = ["crates-search", "rust-docs"]  # allowlist; empty = load all
      disabled = ["experimental-thing"]          # denylist

      [extensions.defaults]
      network              = false
      commands             = false
      max_memory_mb        = 64
      max_execution_time_s = 30

      [extensions.per_extension."rust-docs"]
      network       = true
      max_memory_mb = 128
      ```
    - `ExtensionLoader::discover` respects the enabled/disabled lists
    - Per-extension config is passed to `ExtensionRuntime` at construction time

13. **Extension loading in `App::build`.**
    - After tool registration (phase 7) and context file scanning (phase 8), add:
      ```rust
      // Phase 4: load extensions
      let extensions = ExtensionLoader::new(&sandbox, &config.extensions)
          .load_all(approval_gate_ref, redactor_ref)
          .await?;
      for ext in extensions {
          ext.register_tools(&mut tool_registry);
          // DenoObserver stored on App for AgentObserver dispatch
      }
      ```
    - Extension tool schemas are included in `tool_schemas` before the system prompt is composed

14. **Type definitions for extension authors.**
    - Write `rho-ext/types/rho.d.ts` containing the `rho` namespace declaration and
      `ExtensionManifest` / `ToolDefinition` / `ExtensionHooks` / `CommandDefinition` interfaces
    - Ship to `~/.rho/types/rho.d.ts` on first run (alongside the binary)
    - Include JSDoc comments for discoverability in any TypeScript-aware editor

15. **Custom slash commands.**
    - Extensions register commands via the `commands` array in their default export
    - Commands are collected during loading and stored on `App`
    - REPL mode: `/command_name [args]` dispatches to the extension thread (fire-and-forget)
    - RPC mode: `{"type":"command","name":"...","args":"..."}` dispatches the same way,
      emits a `response` event when done

16. **Hot reload.**
    - `/reload` command in REPL mode; `{"type":"reload_extensions"}` in RPC mode
    - Compare file mtimes against the values recorded at load time
    - For changed or new files: drop the `ExtensionRuntime` sender (thread exits cleanly),
      join the thread, re-transpile, re-spawn, re-register tools via `ToolRegistry::replace`
    - Unchanged extensions: no-op (isolate is preserved)
    - Log which extensions were reloaded, which were unchanged, which failed

17. **Prompt composition with budget awareness.**
    - `compose_full_system_prompt` already assembles base + context files + guidance
    - Extend it to include extension tool schemas as a clearly delimited section
    - Measure token cost of each layer via `HeuristicEstimator` and log the breakdown
      at startup: `"System prompt: 8,200 / 32,768 tokens (25%). base=2,230 AGENTS.md=2,465 ..."`
    - Warn (via `ReplPresenter` / `RpcPresenter`) when the system prompt exceeds a
      configurable fraction of the token budget (default: 50%)
    - Controlled by `[context] prompt_budget_warning_threshold` in config

18. **REPL polish.**
    - Improve tool call display (name, arguments preview, result summary on one line)
    - Better error messages — no raw `?` panics reaching the user; all errors go through
      `ReplPresenter` with context
    - Session status display after each turn (model, context %, active tool count)
    - File-based log rotation in `logs/` (`tracing-appender` already in place)

19. **RPC mode polish.**
    - Validate all event types against the protocol table in `ARCHITECTURE.md`
    - Ensure extension events (`tool_call`, `tool_result`, `tool_denied`) are emitted correctly
      for extension-registered tools (they go through `RpcObserver` like any built-in tool)
    - Add `{"type":"reload_extensions"}` command to the RPC dispatcher

20. **Snapshot test for composed system prompt.**
    - Fixture: fixed tool registry (built-ins only), no context files, default config
    - Assert the composed system prompt is byte-for-byte stable across runs
    - Store expected output in `rho-core/tests/fixtures/prompts/composed_full.md`
    - Assert token cost is below a configurable bound (guards against silent prompt bloat)

21. **Test suite.**
    - Transpilation tests: valid `.ts`, syntax error, import statement, async function
    - Extension loading tests: valid manifest, missing `name`, missing `tools`, wrong types
    - `DenoTool` tests: argument round-trip, success result, error result, cancellation, timeout
    - `DenoObserver` tests: fire-and-forget hook invocation, intercept allow, intercept block,
      intercept timeout (fails open)
    - Sandbox tests: network denied when `network = false`, file path outside sandbox rejected,
      heap limit enforced
    - Config tests: enable/disable by name, per-extension permissions applied
    - Hot reload tests: modified file reloads isolate, new file discovered, unchanged file preserved
    - `ToolRegistry::replace` tests: replaces existing, inserts new
    - `on_tool_call_intercept` tests: allow path, block path, observer composition (multiple observers)

22. **Performance profiling.**
    - Measure and document: V8 isolate startup cost, extension load time (transpile + load),
      per-tool-call overhead vs. built-in tools, memory per isolate, hot reload time for N extensions
    - Add a `rho-bench` task for extension tool call throughput
    - Document findings and any tuning applied in a `perf-notes.md` under this phase directory
