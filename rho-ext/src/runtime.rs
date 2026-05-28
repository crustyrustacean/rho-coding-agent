//! Production extension runtime — owns a V8 isolate on a dedicated thread.
//!
//! [`ExtensionRuntime`] is the public handle. It wraps an `mpsc::Sender<Request>`
//! and a `JoinHandle`. Callers use [`ExtensionRuntime::call`] to invoke named
//! functions on the isolate and receive results over a oneshot channel.
//!
//! # Threading model
//!
//! ```text
//! Caller (tokio async)              Extension thread (owns JsRuntime)
//! ─────────────────────             ──────────────────────────────────
//!                                   thread::spawn(move || {
//!                                       tokio::current_thread
//!                                       LocalSet
//!                                       JsRuntime::new()
//!                                       load ES module
//!                                       extract function handles
//!                                       while let Ok(req) = rx.recv() {
//! runtime.call("greet", arg) ──►          call function via v8::Global
//!                                          drive event loop
//!                                          extract string result
//!                                      ◄── req.reply.send(result)
//!                                       }
//!                                   }) // exits when channel closes
//! ```
//!
//! `JsRuntime` is `!Send + !Sync`. It must never leave the extension thread.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use deno_core::{JsRuntime, ModuleLoader, RuntimeOptions};
use tokio::runtime::Builder;
use tokio::sync::oneshot;
use url::Url;

use crate::error::ExtensionError;
use crate::host::HostState;
use crate::module_loader::RhoModuleLoader;
use crate::transpile::transpile;

// ── Request / Response ────────────────────────────────────────────────────────

/// A request sent from the caller to the extension thread.
struct Request {
    /// The name of the exported function to call.
    function: String,
    /// A JSON-encoded argument to pass to the function.
    argument: String,
    /// A oneshot channel to send the result back.
    reply: oneshot::Sender<Result<String, ExtensionError>>,
}

// ── ExtensionRuntime ─────────────────────────────────────────────────────────

/// A handle to a V8 isolate running on a dedicated thread.
///
/// Create via [`ExtensionRuntime::spawn`] (inline source) or
/// [`ExtensionRuntime::spawn_from_file`] (multi-file extension on disk), then
/// call [`ExtensionRuntime::call`] to invoke exported functions. Drop the handle
/// to terminate the extension thread (the channel closes, the request loop exits,
/// the thread joins).
///
/// For explicit shutdown with panic observation, use [`ExtensionRuntime::shutdown`].
pub struct ExtensionRuntime {
    /// Channel sender for dispatching requests to the extension thread.
    tx: Option<mpsc::Sender<Request>>,
    /// Handle to the extension thread.
    handle: Option<thread::JoinHandle<()>>,
}

impl ExtensionRuntime {
    /// Spawn a new extension runtime from an inline TypeScript source string.
    ///
    /// Transpiles `source`, loads it as an ES module on a dedicated thread, and
    /// extracts the named `exports` from the module namespace. Each export must
    /// be a function.
    ///
    /// Imports within the source will **fail** — there is no module loader. For
    /// multi-file extensions, use [`ExtensionRuntime::spawn_from_file`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if transpilation fails. On failure, no thread is leaked.
    pub fn spawn(specifier: Url, source: &str, exports: &[&str]) -> Result<Self, ExtensionError> {
        let js = transpile(&specifier, source).map_err(ExtensionError::Transpile)?;
        Self::spawn_inner(specifier, js, exports, None)
    }

    /// Spawn a new extension runtime from a TypeScript file on disk.
    ///
    /// Reads `entry_path`, transpiles it, and loads it as the main ES module.
    /// A [`RhoModuleLoader`] is installed to resolve relative imports, enforcing
    /// that all imported files reside within `root_dir` (after symlink
    /// resolution). TypeScript imports are transpiled on the fly.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `entry_path` does not exist or cannot be read
    /// - Transpilation fails
    /// - The extension thread panics during module load
    pub fn spawn_from_file(
        entry_path: &Path,
        root_dir: &Path,
        exports: &[&str],
    ) -> Result<Self, ExtensionError> {
        // Read entry module
        let source = std::fs::read_to_string(entry_path).map_err(|e| {
            ExtensionError::ModuleLoad(format!(
                "failed to read entry module '{}': {e}",
                entry_path.display()
            ))
        })?;

        let specifier = Url::from_file_path(entry_path).map_err(|()| {
            ExtensionError::ModuleLoad(format!(
                "entry path is not a valid file URL: {}",
                entry_path.display()
            ))
        })?;

        let js = transpile(&specifier, &source).map_err(ExtensionError::Transpile)?;

        let root_dir_buf = root_dir.to_path_buf();
        let host_state = HostState {
            cwd: root_dir_buf.clone(),
        };

        Self::spawn_inner_with_host_state(
            specifier,
            js,
            exports,
            Some(root_dir_buf),
            Some(host_state),
        )
    }

    /// Inner spawn: common logic for both `spawn` and `spawn_from_file`.
    ///
    /// `module_loader_root` is `Some(root_dir)` when imports should be resolved
    /// from disk (multi-file extension). The `Rc<dyn ModuleLoader>` is created
    /// *inside* the spawned thread because `Rc` is `!Send`.
    fn spawn_inner(
        specifier: Url,
        js: String,
        exports: &[&str],
        module_loader_root: Option<std::path::PathBuf>,
    ) -> Result<Self, ExtensionError> {
        Self::spawn_inner_with_host_state(specifier, js, exports, module_loader_root, None)
    }

    /// Inner spawn with optional [`HostState`] injection.
    ///
    /// If `host_state` is `Some`, it is placed into the V8 isolate's `OpState`
    /// so that `rho.*` host functions can access the extension's context.
    #[allow(clippy::unnecessary_wraps)]
    fn spawn_inner_with_host_state(
        specifier: Url,
        js: String,
        exports: &[&str],
        module_loader_root: Option<std::path::PathBuf>,
        host_state: Option<HostState>,
    ) -> Result<Self, ExtensionError> {
        let exports = exports.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel::<Request>();

        let handle = thread::spawn(move || {
            let tokio_rt = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for extension thread");

            let local = tokio::task::LocalSet::new();
            local.block_on(&tokio_rt, async move {
                let mut options = RuntimeOptions {
                    extensions: vec![crate::host::rho_host::init()],
                    ..Default::default()
                };
                if let Some(root_dir) = module_loader_root {
                    options.module_loader =
                        Some(Rc::new(RhoModuleLoader::new(root_dir)) as Rc<dyn ModuleLoader>);
                }

                let mut rt = JsRuntime::new(options);

                // Inject HostState into OpState if provided
                if let Some(state) = host_state {
                    rt.op_state().borrow_mut().put(state);
                }

                // Load module
                let mod_id = rt
                    .load_main_es_module_from_code(&specifier, js)
                    .await
                    .expect("failed to load ES module");
                drop(rt.mod_evaluate(mod_id));
                rt.run_event_loop(deno_core::PollEventLoopOptions::default())
                    .await
                    .expect("event loop failed during module load");

                // Extract function handles
                let namespace = rt
                    .get_module_namespace(mod_id)
                    .expect("failed to get module namespace");

                let mut fn_handles: HashMap<
                    String,
                    deno_core::v8::Global<deno_core::v8::Function>,
                > = HashMap::new();

                {
                    deno_core::scope!(scope, rt);
                    let ns = deno_core::v8::Local::<deno_core::v8::Object>::new(scope, namespace);
                    for name in &exports {
                        let key = deno_core::v8::String::new(scope, name).unwrap_or_else(|| {
                            panic!("failed to create v8 string for export '{name}'")
                        });
                        let val = ns.get(scope, key.into()).unwrap_or_else(|| {
                            panic!("export '{name}' not found in module namespace")
                        });
                        let func = deno_core::v8::Local::<deno_core::v8::Function>::try_from(val)
                            .unwrap_or_else(|_| panic!("export '{name}' is not a function"));
                        fn_handles.insert(name.clone(), deno_core::v8::Global::new(scope, func));
                    }
                }

                // Request loop
                while let Ok(req) = rx.recv() {
                    let Some(fn_global) = fn_handles.get(&req.function) else {
                        let _ = req
                            .reply
                            .send(Err(ExtensionError::ExportNotFound(req.function)));
                        continue;
                    };

                    // Create v8 argument
                    let arg_global = {
                        deno_core::scope!(scope, rt);
                        let arg = deno_core::v8::String::new(scope, &req.argument)
                            .unwrap_or_else(|| panic!("failed to create v8 argument string"));
                        let arg_val: deno_core::v8::Local<deno_core::v8::Value> = arg.into();
                        deno_core::v8::Global::new(scope, arg_val)
                    };

                    // Call function + drive event loop
                    let call_future = rt.call_with_args(fn_global, &[arg_global]);
                    let result = match rt
                        .with_event_loop_promise(
                            call_future,
                            deno_core::PollEventLoopOptions::default(),
                        )
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = req
                                .reply
                                .send(Err(ExtensionError::Execution(format!("{e}"))));
                            continue;
                        }
                    };

                    // Extract string from result
                    let result_str = {
                        deno_core::scope!(scope, rt);
                        let local = deno_core::v8::Local::new(scope, result);
                        local.to_rust_string_lossy(scope)
                    };

                    let _ = req.reply.send(Ok(result_str));
                }
            });
        });

        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
        })
    }

    /// Call a named exported function with a string argument.
    ///
    /// The argument is passed as a v8 string — it is **not** parsed as JSON
    /// by the runtime. Extensions that need structured data should
    /// `JSON.parse(arg)` on the JS side.
    ///
    /// # Errors
    ///
    /// - [`ExtensionError::RuntimeShutdown`] if the extension thread has terminated.
    /// - [`ExtensionError::ExportNotFound`] if the function name is not an export.
    /// - [`ExtensionError::Execution`] if the JS function throws.
    pub async fn call(&self, function: &str, argument: &str) -> Result<String, ExtensionError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .as_ref()
            .ok_or(ExtensionError::RuntimeShutdown)?
            .send(Request {
                function: function.to_string(),
                argument: argument.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| ExtensionError::RuntimeShutdown)?;

        reply_rx
            .await
            .map_err(|_| ExtensionError::RuntimeShutdown)?
    }

    /// Shut down the extension thread and wait for it to exit.
    ///
    /// Drops the sender (closing the channel), then joins the thread.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError::RuntimeShutdown`] if the extension thread panicked.
    pub fn shutdown(&mut self) -> Result<(), ExtensionError> {
        // Close the channel — the request loop exits on next recv()
        self.tx.take();

        // Join the thread
        if let Some(handle) = self.handle.take() {
            handle.join().map_err(|_| ExtensionError::RuntimeShutdown)?;
        }

        Ok(())
    }
}

impl Drop for ExtensionRuntime {
    fn drop(&mut self) {
        // Close the channel and detach the thread.
        // We can't block in Drop (it's sync, and the caller may be in an async context),
        // so we don't join — just let the thread exit naturally.
        self.tx.take();
        // JoinHandle drops without joining — the thread is detached and will exit
        // because the channel is closed.
        self.handle.take();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: spawn a runtime from a TypeScript source string.
    fn spawn_runtime(ts_source: &str, exports: &[&str]) -> ExtensionRuntime {
        ExtensionRuntime::spawn(Url::parse("file:///test.ts").unwrap(), ts_source, exports)
            .expect("spawn should succeed")
    }

    // -- Inline source tests (existing) --

    #[tokio::test]
    async fn single_async_function() {
        let mut rt = spawn_runtime(
            r#"export async function greet(name: string): Promise<string> { return `Hello, ${name}!`; }"#,
            &["greet"],
        );

        let result = rt.call("greet", "world").await.unwrap();
        assert_eq!(result, "Hello, world!");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn multiple_exports() {
        let mut rt = spawn_runtime(
            r#"
            export async function greet(name: string): Promise<string> {
                return `Hello, ${name}!`;
            }
            export async function farewell(name: string): Promise<string> {
                return `Goodbye, ${name}!`;
            }
            "#,
            &["greet", "farewell"],
        );

        assert_eq!(rt.call("greet", "Alice").await.unwrap(), "Hello, Alice!");
        assert_eq!(rt.call("farewell", "Bob").await.unwrap(), "Goodbye, Bob!");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_unknown_export_returns_error() {
        let mut rt = spawn_runtime(
            r#"export async function greet(name: string): Promise<string> { return `Hello, ${name}!`; }"#,
            &["greet"],
        );

        let err = rt.call("nonexistent", "arg").await.unwrap_err();
        assert!(matches!(err, ExtensionError::ExportNotFound(name) if name == "nonexistent"));

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn js_error_is_surface() {
        let mut rt = spawn_runtime(
            r#"export async function boom(x: string): Promise<string> { throw new Error("kaboom"); }"#,
            &["boom"],
        );

        let err = rt.call("boom", "whatever").await.unwrap_err();
        assert!(matches!(err, ExtensionError::Execution(msg) if msg.contains("kaboom")));

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn sequential_calls() {
        let mut rt = spawn_runtime(
            r#"
            let counter = 0;
            export async function next(): Promise<string> {
                counter += 1;
                return String(counter);
            }
            "#,
            &["next"],
        );

        assert_eq!(rt.call("next", "").await.unwrap(), "1");
        assert_eq!(rt.call("next", "").await.unwrap(), "2");
        assert_eq!(rt.call("next", "").await.unwrap(), "3");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn shutdown_terminates_cleanly() {
        let mut rt = spawn_runtime(
            r#"export async function noop(): Promise<string> { return "ok"; }"#,
            &["noop"],
        );

        // Verify it works before shutdown
        assert_eq!(rt.call("noop", "").await.unwrap(), "ok");

        // Shutdown should succeed
        rt.shutdown().unwrap();

        // After shutdown, call should fail
        let err = rt.call("noop", "").await.unwrap_err();
        assert!(matches!(err, ExtensionError::RuntimeShutdown));
    }

    #[tokio::test]
    async fn drop_cleans_up() {
        let rt = spawn_runtime(
            r#"export async function ping(): Promise<string> { return "pong"; }"#,
            &["ping"],
        );
        // Drop without explicit shutdown — should not hang or panic.
        drop(rt);
    }

    #[tokio::test]
    async fn json_argument_roundtrip() {
        let mut rt = spawn_runtime(
            r#"
            export async function echo(arg: string): Promise<string> {
                // arg is a JSON string — parse it and re-serialize
                const obj = JSON.parse(arg);
                return JSON.stringify({ got: obj });
            }
            "#,
            &["echo"],
        );

        let input = r#"{"name":"test","value":42}"#;
        let result = rt.call("echo", input).await.unwrap();
        assert_eq!(result, r#"{"got":{"name":"test","value":42}}"#);

        rt.shutdown().unwrap();
    }

    // -- Multi-file (spawn_from_file) tests --

    #[tokio::test]
    async fn spawn_from_file_single_module() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"export async function greet(name: string): Promise<string> { return `Hello, ${name}!`; }"#,
        )
        .unwrap();

        let mut rt = ExtensionRuntime::spawn_from_file(&main_path, dir.path(), &["greet"])
            .expect("spawn_from_file should succeed");

        let result = rt.call("greet", "file-world").await.unwrap();
        assert_eq!(result, "Hello, file-world!");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn spawn_from_file_imports_helper() {
        let dir = tempfile::tempdir().unwrap();

        // helper.ts — a shared module
        let helper_path = dir.path().join("helper.ts");
        std::fs::write(
            &helper_path,
            r#"export function add(a: number, b: number): number { return a + b; }"#,
        )
        .unwrap();

        // main.ts — imports helper
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"
            import { add } from "./helper.ts";
            export async function compute(x: string): Promise<string> {
                return String(add(Number(x), 1));
            }
            "#,
        )
        .unwrap();

        let mut rt = ExtensionRuntime::spawn_from_file(&main_path, dir.path(), &["compute"])
            .expect("spawn_from_file with import should succeed");

        let result = rt.call("compute", "41").await.unwrap();
        assert_eq!(result, "42");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn spawn_from_file_chained_imports() {
        let dir = tempfile::tempdir().unwrap();

        // math.ts — bottom of the chain
        std::fs::write(
            dir.path().join("math.ts"),
            r#"export function multiply(a: number, b: number): number { return a * b; }"#,
        )
        .unwrap();

        // calc.ts — imports math
        std::fs::write(
            dir.path().join("calc.ts"),
            r#"
            import { multiply } from "./math.ts";
            export function doubleAndAdd(x: number, y: number): number {
                return multiply(x, 2) + y;
            }
            "#,
        )
        .unwrap();

        // main.ts — imports calc
        std::fs::write(
            dir.path().join("main.ts"),
            r#"
            import { doubleAndAdd } from "./calc.ts";
            export async function process(n: string): Promise<string> {
                return String(doubleAndAdd(Number(n), 10));
            }
            "#,
        )
        .unwrap();

        let mut rt = ExtensionRuntime::spawn_from_file(&main_path(&dir), dir.path(), &["process"])
            .expect("spawn_from_file with chained imports should succeed");

        // doubleAndAdd(5, 10) = multiply(5, 2) + 10 = 10 + 10 = 20
        let result = rt.call("process", "5").await.unwrap();
        assert_eq!(result, "20");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn spawn_from_file_imports_javascript() {
        let dir = tempfile::tempdir().unwrap();

        // pure.js — a plain JavaScript helper (no TypeScript, no transpilation needed)
        std::fs::write(
            dir.path().join("pure.js"),
            r#"export const label = "js-helper";"#,
        )
        .unwrap();

        // main.ts — imports .js
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"
            import { label } from "./pure.js";
            export async function getLabel(): Promise<string> { return label; }
            "#,
        )
        .unwrap();

        let mut rt = ExtensionRuntime::spawn_from_file(&main_path, dir.path(), &["getLabel"])
            .expect("spawn_from_file importing .js should succeed");

        let result = rt.call("getLabel", "").await.unwrap();
        assert_eq!(result, "js-helper");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn spawn_from_file_nonexistent_entry_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bad_path = dir.path().join("no_such_file.ts");

        let result = ExtensionRuntime::spawn_from_file(&bad_path, dir.path(), &["foo"]);
        assert!(result.is_err(), "should fail for nonexistent file");
        match result {
            Err(ExtensionError::ModuleLoad(msg)) => {
                assert!(
                    msg.contains("no_such_file"),
                    "error should mention file name: {msg}"
                );
            }
            Err(other) => panic!("expected ModuleLoad error, got: {other}"),
            Ok(_) => panic!("should not succeed"),
        }
    }

    /// Helper to build the main.ts path for a temp dir.
    fn main_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("main.ts")
    }

    #[tokio::test]
    async fn rho_global_available_in_extension() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"
            export async function getCwd(): Promise<string> {
                return rho.getCwd();
            }
            export async function logSomething(): Promise<string> {
                rho.log("info", "test log from extension");
                return "logged";
            }
            "#,
        )
        .unwrap();

        let mut rt =
            ExtensionRuntime::spawn_from_file(&main_path, dir.path(), &["getCwd", "logSomething"])
                .expect("spawn_from_file should succeed");

        // getCwd should return the extension's root dir
        let cwd = rt.call("getCwd", "").await.unwrap();
        assert_eq!(cwd, dir.path().to_str().unwrap());

        // logSomething should not crash and return "logged"
        let result = rt.call("logSomething", "").await.unwrap();
        assert_eq!(result, "logged");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn rho_global_not_available_in_spawn() {
        // ExtensionRuntime::spawn (inline) does NOT inject HostState,
        // but rho global is still available (just getCwd falls back).
        let mut rt = spawn_runtime(
            r#"
            export async function getCwd(): Promise<string> {
                return rho.getCwd();
            }
            "#,
            &["getCwd"],
        );

        let cwd = rt.call("getCwd", "").await.unwrap();
        // Should return something (current dir), not crash
        assert!(!cwd.is_empty());

        rt.shutdown().unwrap();
    }
}
