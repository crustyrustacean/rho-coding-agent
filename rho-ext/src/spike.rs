//! Architecture spike — validate the threading model and `deno_core` 0.401.0 API.
//!
//! # Purpose
//!
//! Before committing to any production architecture, this spike proves that the
//! full vertical slice works with the *real* crate APIs:
//!
//! 1. Transpile TypeScript → JavaScript via `deno_ast 0.53.2`
//! 2. Load the JS as an ES module into a `JsRuntime`
//! 3. Call an async function on a dedicated thread
//! 4. Return the result to the calling thread over a channel
//!
//! # Why this spike exists
//!
//! `JsRuntime` uses `Rc<RefCell<…>>` internally and is `!Send`. The
//! `Arc<tokio::sync::Mutex<JsRuntime>>` pattern from the pi-brain design doc
//! **will not compile**. This spike confirms the only viable pattern — a
//! dedicated OS thread per isolate with channel-based communication — and
//! uncovers any other API surprises in `deno_core` 0.401.0 before they
//! propagate into the rest of the crate.
//!
//! # What to implement
//!
//! ## Step 1: TypeScript source
//!
//! Hard-code a trivial async function:
//!
//! ```typescript
//! export async function greet(name: string): Promise<string> {
//!     return `Hello, ${name}!`;
//! }
//! ```
//!
//! ## Step 2: Transpile via `deno_ast 0.53.2`
//!
//! ```ignore
//! use deno_ast::{EmitOptions, MediaType, ParseParams, SourceTextInfo, TranspileOptions};
//!
//! let parsed = deno_ast::parse_module(ParseParams {
//!     specifier: "file:///spike.ts".into(),
//!     text: SourceTextInfo::from_string(ts_source),
//!     media_type: MediaType::TypeScript,
//!     capture_tokens: false,
//!     scope_analysis: false,
//!     maybe_syntax: None,
//! })?;
//! let transpiled = parsed.transpile(
//!     &TranspileOptions::default(),
//!     &EmitOptions::default(),
//! )?;
//! let js_source = transpiled.into_source().text;
//! ```
//!
//! Verify the output is valid JavaScript (no TypeScript syntax remaining).
//!
//! ## Step 3: Create a `JsRuntime` on a dedicated thread
//!
//! `JsRuntime: !Send` means it must live on one thread for its entire lifetime.
//! The pattern:
//!
//! - Spawn a `std::thread::spawn` that creates a `tokio::task::LocalSet`
//! - Create `JsRuntime::new(RuntimeOptions { … })` on that local set
//! - Loop on an `mpsc::Receiver<Request>` where `Request` carries a function
//!   name, JSON args, and a `oneshot::Sender` for the reply
//!
//! The public handle is a cloneable `mpsc::Sender<Request>` + a `JoinHandle`.
//!
//! ## Step 4: Load the module
//!
//! On the extension thread, use:
//!
//! ```ignore
//! let mod_id = rt.load_main_es_module_from_code(&specifier, js_source).await?;
//! let evaluate_future = rt.mod_evaluate(mod_id);
//! rt.run_event_loop(Default::default()).await?;
//! ```
//!
//! Then extract the exported function handle from the module namespace:
//!
//! ```ignore
//! let namespace = rt.get_module_namespace(mod_id)?;
//! // Look up "greet" on the namespace object → v8::Global<v8::Function>
//! ```
//!
//! ## Step 5: Call the async function and return the result
//!
//! From the calling thread (e.g. a test), send a `Request` over the channel.
//! On the extension thread:
//!
//! ```ignore
//! // NOTE: call_with_args_and_await is deprecated in 0.401.0.
//! // Use call_with_args (non-deprecated) + run_event_loop:
//! let call_future = rt.call_with_args(&fn_handle, &[args_v8]);
//! rt.with_event_loop_promise(call_future, Default::default()).await?;
//! // Convert v8 result to String, send back via oneshot
//! ```
//!
//! The calling thread awaits the `oneshot::Receiver<String>`.
//!
//! ## Step 6: Assert
//!
//! A `#[test] fn spike_transpile_load_call()` that:
//! - Transpiles the TypeScript source
//! - Spawns the extension thread
//! - Sends `("greet", r#""world""#)`
//! - Receives `"Hello, world!"` on the oneshot channel
//! - Drops the sender (shuts down the extension thread)
//!
//! # Expected size
//!
//! ~150–200 lines. This file is disposable — delete it or keep it as a
//! documented example once the spike passes and the production crate
//! infrastructure is built.
//!
//! # Things to watch for
//!
//! - `JsRuntime::new` may require specific `RuntimeOptions` fields — check what
//!   the minimal set is (no extensions, no module loader, no snapshot).
//! - `get_module_namespace` may return a `v8::Global<v8::Object>` that must be
//!   accessed within a `HandleScope` — figure out the right scope lifetime.
//! - `run_event_loop` may block indefinitely if there are pending microtasks —
//!   confirm it terminates after the function resolves.
//! - If `load_main_es_module_from_code` doesn't work with a `file://` specifier,
//!   try a custom specifier scheme.
//!
//! # API notes (verified from docs.rs / `deno_core` 0.401.0)
//!
//! - `JsRuntime` is `!Send + !Sync + !RefUnwindSafe + !UnwindSafe` — confirmed
//!   from the Auto Trait Implementations on docs.rs. This means the dedicated-
//!   thread-per-isolate pattern is mandatory.
//! - `call_and_await` and `call_with_args_and_await` are **deprecated** in
//!   0.401.0. Use `call` and `call_with_args` instead. These return
//!   `impl Future<Output = Result<Global<Value>, Box<JsError>>>`, but the docs
//!   state: "The event loop must be polled separately for this future to
//!   resolve." So the pattern is:
//!   ```ignore
//!   let future = rt.call(&fn_handle);       // does NOT drive the event loop
//!   rt.run_event_loop(Default::default()).await?;  // drives until the call resolves
//!   let result = future.await?;              // now the result is available
//!   ```
//!   Or use `with_event_loop_promise` / `with_event_loop_future` to run them
//!   concurrently.
//! - `execute_script` returns `Result<Global<Value>, Box<JsError>>` and is
//!   synchronous-only — it cannot drive async functions. ES modules with async
//!   exports must use `load_main_es_module_from_code` + `mod_evaluate` +
//!   `run_event_loop`.
//!
//! # After the spike
//!
//! If the pattern works, the production `ExtensionRuntime` in `runtime.rs`
//! generalises this spike: N functions instead of one, `OpState` injection,
//! host function ops, timeout handling, and graceful shutdown.

use deno_ast::{EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions};
use tokio::sync::oneshot;
use url::Url;

#[allow(dead_code, clippy::missing_docs_in_private_items)]
const GREET_FN: &str =
    r"export async function greet(name: string): Promise<string> { return `Hello, ${name}!`; }";

#[allow(dead_code, clippy::missing_docs_in_private_items)]
struct Request {
    arg: String,
    reply: oneshot::Sender<String>,
}

#[allow(dead_code, clippy::missing_docs_in_private_items)]
fn transpile(source: &str) -> String {
    let parsed = deno_ast::parse_module(ParseParams {
        specifier: Url::parse("file:///spike.ts").unwrap(),
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .unwrap();
    let transpiled = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions::default(),
            &EmitOptions::default(),
        )
        .unwrap();
    transpiled.into_source().text
}

#[cfg(test)]
mod tests {

    use std::sync::mpsc;
    use std::thread;

    use deno_core::{JsRuntime, RuntimeOptions};
    use tokio::runtime::Builder;

    use super::*;
    use deno_core::v8;

    #[tokio::test]
    async fn spike_transpile_load_call() {
        // Step 1 & 2: transpile TypeScript → JavaScript
        let js = transpile(GREET_FN);
        assert!(js.contains("Hello"));

        // Step 3: spawn the extension thread
        let (tx, rx) = mpsc::channel::<Request>();
        let handle = thread::spawn(move || {
            // This thread owns JsRuntime for its entire lifetime.
            // JsRuntime is !Send — it must never leave this thread.
            let tokio_rt = Builder::new_current_thread().enable_all().build().unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&tokio_rt, async {
                let mut rt = JsRuntime::new(RuntimeOptions::default());
                let specifier = Url::parse("file:///spike.ts").unwrap();

                // Step 4: load the transpiled JS as an ES module
                let mod_id = rt
                    .load_main_es_module_from_code(&specifier, js)
                    .await
                    .unwrap();
                drop(rt.mod_evaluate(mod_id));
                rt.run_event_loop(deno_core::PollEventLoopOptions::default())
                    .await
                    .unwrap();

                // Extract the "greet" function from the module namespace
                let namespace = rt.get_module_namespace(mod_id).unwrap();
                let greet_global = {
                    deno_core::scope!(scope, rt);
                    let ns = v8::Local::<v8::Object>::new(scope, namespace);
                    let key = v8::String::new(scope, "greet").unwrap();
                    let val = ns.get(scope, key.into()).unwrap();
                    let func = v8::Local::<v8::Function>::try_from(val).unwrap();
                    v8::Global::new(scope, func)
                };

                // Step 5: receive requests and call the function
                while let Ok(req) = rx.recv() {
                    let arg_global = {
                        deno_core::scope!(scope, rt);
                        let arg = v8::String::new(scope, &req.arg).unwrap();
                        let arg_val: v8::Local<v8::Value> = arg.into();
                        v8::Global::new(scope, arg_val)
                    };

                    let call_future = rt.call_with_args(&greet_global, &[arg_global]);
                    let result = rt
                        .with_event_loop_promise(
                            call_future,
                            deno_core::PollEventLoopOptions::default(),
                        )
                        .await
                        .unwrap();

                    let result_str = {
                        deno_core::scope!(scope, rt);
                        let local = v8::Local::new(scope, result);
                        local.to_rust_string_lossy(scope)
                    };
                    let _ = req.reply.send(result_str);
                }
            });
        });

        // Test is now a client: send a request, await the reply
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Request {
            arg: "world".into(),
            reply: reply_tx,
        })
        .unwrap();

        let result = reply_rx.await.unwrap();
        assert_eq!(result, "Hello, world!");

        // Drop the sender to shut down the extension thread
        drop(tx);
        handle.join().unwrap();
    }
}
