# Plan: General Async Dispatcher + Simplified Op Registration

**Branch:** `improvement/general-async-dispatcher`
**Goal:** Let extension authors write TypeScript using familiar APIs (`fetch`, `setTimeout`, etc.) with all async Rust plumbing handled implicitly by rho.

## Problem Summary

1. **Every async capability needs a dedicated thread.** `HttpExecutor` proves the pattern but is HTTP-specific. Adding WebSocket support means another thread + channel + op boilerplate.
2. **Five files per new op.** Each `rho.*` capability touches `host.rs` (op definition, extension registration, permission wiring), `host_shim.js` (JS wrapper), and sometimes `config.rs`/`runtime.rs`.
3. **Extension authors hallucinate APIs.** The `rho.*` surface is small and non-standard — authors assume `fetch`, `setTimeout`, `crypto` exist.

## Proposed Solution (3 phases)

### Phase 1: General-purpose `AsyncDispatcher` (Rust only, no API change)

Replace the one-off `HttpExecutor` with a shared background executor that any sync op can dispatch async work to.

#### New file: `rho-ext/src/async_dispatcher.rs`

```rust
pub struct AsyncDispatcher {
    tx: mpsc::Sender<AsyncTask>,
    _thread: JoinHandle<()>,
}

// A boxed future that returns a String (or error).
// The dispatcher runs it on its dedicated tokio runtime + threadpool.
struct AsyncTask {
    future: Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send>,
    reply: mpsc::Sender<Result<String, String>>,
}

impl AsyncDispatcher {
    /// Spawn the background thread (called once at runtime init).
    fn new() -> Self { ... }

    /// Dispatch an async future and block until it completes.
    /// Call this from any sync op.
    fn block_on<F>(&self, f: impl Future<Output = Result<String, String>> + Send + 'static) -> Result<String, String> { ... }
}
```

Key properties:
- **Single thread, single runtime, single channel.** All async capabilities share it.
- **Generic.** Takes any `Future<Output = Result<String, String>>`. No type-specific channels.
- **`OnceLock` singleton.** Lazily created, lives for the process lifetime. Same pattern as current `HttpExecutor`.

#### Refactor: Migrate `op_rho_fetch_url`

Rewrite `op_rho_fetch_url` to use `AsyncDispatcher::block_on()` instead of the dedicated `HttpExecutor`. Delete `HttpExecutor`, `HttpRequest`, `http_executor()`. This proves the dispatcher works with a real workload before adding new capabilities.

**Files changed:**
- `rho-ext/src/async_dispatcher.rs` — new
- `rho-ext/src/host.rs` — delete `HttpExecutor`, rewrite `op_rho_fetch_url`
- `rho-ext/src/lib.rs` — add `pub mod async_dispatcher`

**Tests:** Existing `op_rho_fetch_url` tests should pass unchanged. Add unit tests for `AsyncDispatcher` (spawn, dispatch, shutdown).

---

### Phase 2: Expose Deno standard APIs via adapted extensions

Wire up selected Deno extension crates so that extension code can use `fetch()`, `setTimeout()`, `crypto.subtle.digest()`, etc. directly — without `rho.*` wrappers.

#### Candidate Deno extensions (ordered by impact)

| Deno crate | API surface | Effort | Requires dispatcher? |
|---|---|---|---|
| `deno_timers` | `setTimeout`, `setInterval`, `clearTimeout`, `clearInterval` | Low | Yes (timer ops are async) |
| `deno_fetch` | `fetch()` | Medium | Yes (HTTP) |
| `deno_webidl` | Supporting types for above | Low (dep) | No |
| `deno_url` | `URL`, `URLSearchParams` | Low | No |
| `deno_crypto` | `crypto.subtle.*` | Medium | Yes (async hash/verify) |
| `deno_console` | Better `console.*` (already have basic console) | Low | No |

#### Approach: Shim layer, not forked Deno crates

We can't use the Deno crates as-is because their ops use `#[op2(async)]` internally, which hits the V8 FFI unwind issue. Instead:

1. **For sync-capable APIs** (`deno_url`, `deno_console`): Register directly. These are sync ops and work fine.

2. **For async APIs** (`deno_timers`, `deno_fetch`, `deno_crypto`): Write thin **sync wrapper ops** that dispatch to the `AsyncDispatcher`. The wrapper ops:
   - Extract params from V8 (sync)
   - Dispatch the actual async logic to `AsyncDispatcher::block_on()`
   - Return the result to V8 (sync)
   
   This is the same pattern as `op_rho_fetch_url`, but generalized.

3. **JS shims** on `globalThis` (not `rho.*`): These go on `globalThis.setTimeout`, `globalThis.fetch`, etc. — standard Web API placement. No `rho.` prefix. Extension authors don't need to know about rho at all.

#### Permission gating

Add new permission flags to `ExtensionPermissions`:

```rust
pub struct ExtensionPermissions {
    pub network: Option<bool>,      // existing — gates fetch()
    pub commands: Option<bool>,     // existing
    pub timers: Option<bool>,       // NEW — gates setTimeout/setInterval
    pub crypto: Option<bool>,       // NEW — gates crypto.subtle
    // ...
}
```

Each op checks the permission before dispatching. Default: all off (explicit opt-in).

**Files changed:**
- `rho-ext/src/deno_shims.rs` — new (shim ops + JS for standard APIs)
- `rho-ext/src/host.rs` — add `AsyncDispatcher` to `HostState`
- `rho-ext/src/runtime.rs` — register shim extension alongside `rho_host`
- `rho-core/src/config.rs` — add `timers`, `crypto` permission fields
- `rho-ext/Cargo.toml` — add `deno_url`, `deno_console`, `deno_timers` deps

**Tests:** Per-API tests — `setTimeout` fires callback, `fetch()` returns data, `crypto.subtle.digest` hashes correctly.

---

### Phase 3: `dispatch_op!` macro for zero-boilerplate op registration

A procedural or declarative macro that collapses the five-file ceremony into one declaration.

#### Macro design

```rust
// In host.rs or a dedicated ops module:
dispatch_op! {
    /// `rho.fetchUrl(opts)` — fetch a URL.
    fn fetch_url(
        name = "fetchUrl",
        permission = allow_network,
        params = { opts: String },
        returns = String,
    ) {
        // async body — dispatcher handles it
        async move {
            let req = build_request(&opts)?;
            let client = reqwest::Client::new();
            let resp = client.execute(req).await?;
            Ok(resp.text().await?)
        }
    }
}
```

This expands to:
- The `#[op2]` sync fn that checks permissions and dispatches to `AsyncDispatcher`
- Registration in a vec that gets wired into `deno_core::extension!()`
- A JS wrapper entry (auto-generated string, collected at macro expansion)
- Permission field in `HostState` (if new)

#### Simpler variant for sync ops

```rust
dispatch_sync_op! {
    fn get_cwd(state) -> String {
        state.borrow::<HostState>().cwd.to_str().unwrap().to_string()
    }
}
```

**Files changed:**
- `rho-ext/src/macros.rs` — new (or `rho-ext/src/dispatch_op.rs`)
- `rho-ext/src/host.rs` — refactor existing ops to use macros
- `rho-ext/src/host_shim.js` — generated by macro (or kept manual for sync ops)

**Tests:** Existing tests pass unchanged after refactor.

---

## Execution Order

```
Phase 1 (AsyncDispatcher)
  ├── Create async_dispatcher.rs
  ├── Migrate HttpExecutor → AsyncDispatcher
  ├── Tests: existing + new dispatcher unit tests
  └── PR / merge

Phase 2 (Deno standard APIs)
  ├── Add deno_url + deno_console (sync, no dispatcher needed)
  ├── Add deno_timers (async, needs dispatcher)
  ├── Add deno_fetch (async, needs dispatcher, replaces rho.fetchUrl)
  ├── Add deno_crypto (async, needs dispatcher)
  ├── Update permissions in config
  ├── Tests: per-API
  └── PR / merge

Phase 3 (dispatch_op! macro)
  ├── Define macro
  ├── Refactor existing ops
  ├── Verify all tests pass
  └── PR / merge
```

## Success Criteria

After all three phases, an extension author should be able to write:

```typescript
// ~/.rho/extensions/my-extension/main.ts
export default {
  name: "my-extension",
  tools: [{
    name: "fetch-data",
    description: "Fetch data from an API",
    risk: "network" as const,
    parameters: { url: { type: "string", description: "URL to fetch" } },
    execute: async (args: string) => {
      const { url } = JSON.parse(args);
      const res = await fetch(url);           // standard Web API, not rho.fetchUrl
      const data = await res.json();
      return JSON.stringify(data);
    },
  }],
};
```

With config:
```toml
[extensions.defaults]
network = true  # gates fetch()
```

No `rho.*` needed for standard operations. The Rust plumbing is implicit.

## Open Questions

1. **deno_fetch vs. keeping rho.fetchUrl?** Should we expose `fetch()` on globalThis and deprecate `rho.fetchUrl`, or keep both? Proposal: expose `fetch()`, keep `rho.fetchUrl` as alias for backward compat.

2. **How many Deno crates to include?** Start with `deno_timers` + `deno_fetch` (highest impact), then `deno_crypto` if there's demand. `deno_fs` is lower priority since we already have `rho.readFile`/`rho.writeFile` with sandboxing.

3. **Macro: proc macro vs. declarative?** Declarative (`macro_rules!`) is simpler but can't generate JS strings easily. Proc macro (`proc-macro2`) can generate both Rust + JS but adds a compile-time dependency. Start with declarative, move to proc if needed.

4. **Error semantics across the boundary.** Currently ops return `__ERROR__` prefixed strings. Should the dispatcher use proper `Result` types and let the JS shim do error translation? This is a pre-existing design choice — we can improve it but it's orthogonal to the dispatcher.
