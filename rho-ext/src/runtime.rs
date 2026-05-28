//! Production extension runtime — owns a V8 isolate on a dedicated thread.
//!
//! [`ExtensionRuntime`] is the public handle. It wraps an `mpsc::Sender<Request>`
//! and a `JoinHandle`. The extension's manifest is extracted during spawn and
//! available via [`ExtensionRuntime::manifest`]. Use [`ExtensionRuntime::call_tool`],
//! [`ExtensionRuntime::call_hook`], or [`ExtensionRuntime::call_command`] to invoke
//! functions declared in the manifest.
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
//!                                       extract manifest + function handles
//!                                       ── send manifest to caller ──►
//!                                       while let Ok(req) = rx.recv() {
//! rt.call_tool("greet", arg) ──►        dispatch to tool/hook/command fn
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

use deno_core::v8::{self, Global, HandleScope, Local, Object, PinnedRef};
use deno_core::{JsRuntime, ModuleLoader, RuntimeOptions};
use tokio::runtime::Builder;
use tokio::sync::oneshot;
use url::Url;

use crate::error::ExtensionError;
use crate::host::HostState;
use crate::manifest::{self, LoadedExtension, LoadedHooks};
use crate::module_loader::RhoModuleLoader;
use crate::transpile::transpile;

// ── Request / Response ────────────────────────────────────────────────────────

/// Which function to call on the extension thread.
enum CallTarget {
    /// Call a tool's `execute` function.
    Tool(String),
    /// Call a hook function by name.
    Hook(String),
    /// Call a command's `handler` function.
    Command(String),
}

/// A request sent from the caller to the extension thread.
struct Request {
    /// Which function to call.
    target: CallTarget,
    /// A string argument to pass to the function.
    argument: String,
    /// A oneshot channel to send the result back.
    reply: oneshot::Sender<Result<String, ExtensionError>>,
}

/// Function handles stored on the extension thread, keyed by name.
struct FunctionHandles {
    /// Tool name → `execute` function.
    tools: HashMap<String, Global<v8::Function>>,
    /// Hook name → hook function (e.g. "onLoad", "onToolCall").
    hooks: HashMap<String, Global<v8::Function>>,
    /// Command name → `handler` function.
    commands: HashMap<String, Global<v8::Function>>,
}

// ── ExtensionRuntime ─────────────────────────────────────────────────────────

/// A handle to a V8 isolate running on a dedicated thread.
///
/// Create via [`ExtensionRuntime::spawn`] (inline source) or
/// [`ExtensionRuntime::spawn_from_file`] (multi-file extension on disk), then
/// use [`ExtensionRuntime::call_tool`] / [`ExtensionRuntime::call_hook`] /
/// [`ExtensionRuntime::call_command`] to invoke functions. Drop the handle
/// to terminate the extension thread (the channel closes, the request loop exits,
/// the thread joins).
///
/// For explicit shutdown with panic observation, use [`ExtensionRuntime::shutdown`].
pub struct ExtensionRuntime {
    /// Channel sender for dispatching requests to the extension thread.
    tx: Option<mpsc::Sender<Request>>,
    /// Handle to the extension thread.
    handle: Option<thread::JoinHandle<()>>,
    /// The extracted extension manifest.
    manifest: LoadedExtension,
}

impl ExtensionRuntime {
    /// Spawn a new extension runtime from an inline TypeScript source string.
    ///
    /// Transpiles `source`, loads it as an ES module on a dedicated thread,
    /// extracts the `export default { ... }` manifest, and returns the runtime
    /// handle with the manifest metadata.
    ///
    /// Imports within the source will **fail** — there is no module loader. For
    /// multi-file extensions, use [`ExtensionRuntime::spawn_from_file`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if transpilation fails, the module fails to load, or
    /// manifest extraction fails. On failure, no thread is leaked.
    pub fn spawn(specifier: Url, source: &str) -> Result<Self, ExtensionError> {
        let js = transpile(&specifier, source).map_err(ExtensionError::Transpile)?;
        Self::spawn_inner(specifier, js, None, None)
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
    /// - Manifest extraction fails
    /// - Any tool is missing its `execute` function or command its `handler`
    pub fn spawn_from_file(entry_path: &Path, root_dir: &Path) -> Result<Self, ExtensionError> {
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

        Self::spawn_inner(specifier, js, Some(root_dir_buf), Some(host_state))
    }

    /// Inner spawn: common logic for both `spawn` and `spawn_from_file`.
    ///
    /// Uses a `std::sync::mpsc` channel for the init handshake — the extension
    /// thread sends back the manifest (or an error) before entering the request
    /// loop, so `spawn` blocks until the extension is fully initialized.
    #[allow(clippy::too_many_lines)]
    fn spawn_inner(
        specifier: Url,
        js: String,
        module_loader_root: Option<std::path::PathBuf>,
        host_state: Option<HostState>,
    ) -> Result<Self, ExtensionError> {
        let (init_tx, init_rx) =
            std::sync::mpsc::channel::<Result<LoadedExtension, ExtensionError>>();
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
                let mod_id = match rt.load_main_es_module_from_code(&specifier, js).await {
                    Ok(id) => id,
                    Err(e) => {
                        let _ = init_tx.send(Err(ExtensionError::ModuleLoad(format!("{e}"))));
                        return;
                    }
                };
                drop(rt.mod_evaluate(mod_id));
                if let Err(e) = rt
                    .run_event_loop(deno_core::PollEventLoopOptions::default())
                    .await
                {
                    let _ = init_tx.send(Err(ExtensionError::ModuleLoad(format!(
                        "event loop error: {e}"
                    ))));
                    return;
                }

                // Extract manifest + function handles
                let namespace = match rt.get_module_namespace(mod_id) {
                    Ok(ns) => ns,
                    Err(e) => {
                        let _ = init_tx.send(Err(ExtensionError::ModuleLoad(format!(
                            "failed to get module namespace: {e}"
                        ))));
                        return;
                    }
                };

                let (manifest, fn_handles) = {
                    deno_core::scope!(scope, rt);
                    let ns = v8::Local::<v8::Object>::new(scope, namespace);

                    // Extract manifest metadata
                    let loaded = match manifest::extract_manifest(scope, ns) {
                        Ok(m) => m,
                        Err(e) => {
                            let _ = init_tx.send(Err(ExtensionError::Manifest(e)));
                            return;
                        }
                    };

                    // Extract function handles
                    let handles = match extract_function_handles(scope, ns, &loaded) {
                        Ok(h) => h,
                        Err(e) => {
                            let _ = init_tx.send(Err(e));
                            return;
                        }
                    };

                    (loaded, handles)
                };

                // Send manifest back to caller
                if init_tx.send(Ok(manifest)).is_err() {
                    return;
                }

                // Request loop
                while let Ok(req) = rx.recv() {
                    let fn_global = match &req.target {
                        CallTarget::Tool(name) => fn_handles.tools.get(name),
                        CallTarget::Hook(name) => fn_handles.hooks.get(name),
                        CallTarget::Command(name) => fn_handles.commands.get(name),
                    };

                    let Some(fn_global) = fn_global else {
                        let err = match &req.target {
                            CallTarget::Tool(name) => ExtensionError::ToolNotFound(name.clone()),
                            CallTarget::Hook(name) => ExtensionError::HookNotFound(name.clone()),
                            CallTarget::Command(name) => {
                                ExtensionError::CommandNotFound(name.clone())
                            }
                        };
                        let _ = req.reply.send(Err(err));
                        continue;
                    };

                    // Create v8 argument
                    let arg_global = {
                        deno_core::scope!(scope, rt);
                        let arg = v8::String::new(scope, &req.argument)
                            .unwrap_or_else(|| panic!("failed to create v8 argument string"));
                        let arg_val: v8::Local<v8::Value> = arg.into();
                        Global::new(scope, arg_val)
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
                        let local = v8::Local::new(scope, result);
                        local.to_rust_string_lossy(scope)
                    };

                    let _ = req.reply.send(Ok(result_str));
                }
            });
        });

        // Wait for init result from the extension thread
        let manifest = init_rx
            .recv()
            .map_err(|_| ExtensionError::RuntimeShutdown)??;

        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
            manifest,
        })
    }

    /// Return the extension's manifest metadata.
    pub fn manifest(&self) -> &LoadedExtension {
        &self.manifest
    }

    /// Call a tool's `execute` function with a string argument.
    ///
    /// The argument is passed as a v8 string — it is **not** parsed as JSON
    /// by the runtime. Extensions that need structured data should
    /// `JSON.parse(arg)` on the JS side.
    ///
    /// # Errors
    ///
    /// - [`ExtensionError::RuntimeShutdown`] if the extension thread has terminated.
    /// - [`ExtensionError::ToolNotFound`] if the tool name is not in the manifest.
    /// - [`ExtensionError::Execution`] if the JS function throws.
    pub async fn call_tool(&self, name: &str, argument: &str) -> Result<String, ExtensionError> {
        self.call_internal(CallTarget::Tool(name.to_string()), argument)
            .await
    }

    /// Call a hook function by name with a string argument.
    ///
    /// Hook names correspond to the manifest's `hooks` keys: `"onLoad"`,
    /// `"onToolCall"`, `"onToolResult"`, `"onBeforeModel"`.
    ///
    /// # Errors
    ///
    /// - [`ExtensionError::RuntimeShutdown`] if the extension thread has terminated.
    /// - [`ExtensionError::HookNotFound`] if the hook name is not declared.
    /// - [`ExtensionError::Execution`] if the JS function throws.
    pub async fn call_hook(&self, name: &str, argument: &str) -> Result<String, ExtensionError> {
        self.call_internal(CallTarget::Hook(name.to_string()), argument)
            .await
    }

    /// Call a command's `handler` function with a string argument.
    ///
    /// # Errors
    ///
    /// - [`ExtensionError::RuntimeShutdown`] if the extension thread has terminated.
    /// - [`ExtensionError::CommandNotFound`] if the command name is not in the manifest.
    /// - [`ExtensionError::Execution`] if the JS function throws.
    pub async fn call_command(&self, name: &str, argument: &str) -> Result<String, ExtensionError> {
        self.call_internal(CallTarget::Command(name.to_string()), argument)
            .await
    }

    /// Internal dispatch.
    async fn call_internal(
        &self,
        target: CallTarget,
        argument: &str,
    ) -> Result<String, ExtensionError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .as_ref()
            .ok_or(ExtensionError::RuntimeShutdown)?
            .send(Request {
                target,
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

// ── Function handle extraction ────────────────────────────────────────────────

/// Extract V8 function handles from the manifest's `export default` object.
///
/// After [`manifest::extract_manifest`] has validated the metadata, this function
/// walks the same V8 objects to pull out the actual function references:
/// - Each tool's `execute` function
/// - Each declared hook function
/// - Each command's `handler` function
fn extract_function_handles(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    namespace: Local<Object>,
    manifest: &LoadedExtension,
) -> Result<FunctionHandles, ExtensionError> {
    // Get the default export object
    let default_key = v8::String::new(scope, "default")
        .unwrap_or_else(|| panic!("failed to create v8 string 'default'"));
    let Some(default_val) = namespace.get(scope, default_key.into()) else {
        return Err(ExtensionError::Manifest(
            manifest::ManifestError::NoDefaultExport,
        ));
    };
    let manifest_obj = v8::Local::<Object>::try_from(default_val)
        .map_err(|_| ExtensionError::Manifest(manifest::ManifestError::DefaultExportNotObject))?;

    let tools = extract_tool_handles(scope, manifest_obj, &manifest.tools)?;
    let hooks = extract_hook_handles(scope, manifest_obj, &manifest.hooks);
    let commands = extract_command_handles(scope, manifest_obj, &manifest.commands)?;

    Ok(FunctionHandles {
        tools,
        hooks,
        commands,
    })
}

/// Extract `execute` function handles for each tool.
#[allow(clippy::cast_possible_truncation)]
fn extract_tool_handles(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    manifest_obj: Local<Object>,
    tools: &[crate::manifest::LoadedTool],
) -> Result<HashMap<String, Global<v8::Function>>, ExtensionError> {
    let mut handles = HashMap::new();
    if tools.is_empty() {
        return Ok(handles);
    }

    let tools_key = v8::String::new(scope, "tools").expect("failed to create v8 string 'tools'");
    let Some(tools_v8) = manifest_obj.get(scope, tools_key.into()) else {
        return Err(ExtensionError::Manifest(
            manifest::ManifestError::InvalidField {
                field: "tools".to_string(),
                reason: "tools declared in manifest but missing from V8 object".to_string(),
            },
        ));
    };
    let tools_arr = v8::Local::<v8::Array>::try_from(tools_v8).map_err(|_| {
        ExtensionError::Manifest(manifest::ManifestError::InvalidField {
            field: "tools".to_string(),
            reason: "expected an array".to_string(),
        })
    })?;

    for (i, tool) in tools.iter().enumerate() {
        let Some(element) = tools_arr.get_index(scope, i as u32) else {
            return Err(ExtensionError::Manifest(
                manifest::ManifestError::ToolMissingField {
                    index: i,
                    field: "execute".to_string(),
                },
            ));
        };
        let Ok(tool_obj) = v8::Local::<Object>::try_from(element) else {
            return Err(ExtensionError::Manifest(
                manifest::ManifestError::ToolInvalidField {
                    index: i,
                    field: "execute".to_string(),
                    reason: "tool element is not an object".to_string(),
                },
            ));
        };
        let exec_key = v8::String::new(scope, "execute")
            .unwrap_or_else(|| panic!("failed to create v8 string 'execute'"));
        let Some(exec_val) = tool_obj.get(scope, exec_key.into()) else {
            return Err(ExtensionError::ToolMissingExecute(tool.name.clone()));
        };
        if !exec_val.is_function() {
            return Err(ExtensionError::ToolMissingExecute(tool.name.clone()));
        }
        let exec_fn = v8::Local::<v8::Function>::try_from(exec_val).expect("checked is_function");
        handles.insert(tool.name.clone(), Global::new(scope, exec_fn));
    }

    Ok(handles)
}

/// Extract function handles for declared hooks.
fn extract_hook_handles(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    manifest_obj: Local<Object>,
    hooks_meta: &LoadedHooks,
) -> HashMap<String, Global<v8::Function>> {
    let mut handles = HashMap::new();
    let hooks_key = v8::String::new(scope, "hooks").expect("failed to create v8 string 'hooks'");

    let Some(hooks_val) = manifest_obj.get(scope, hooks_key.into()) else {
        return handles;
    };
    let Ok(hooks_obj) = v8::Local::<Object>::try_from(hooks_val) else {
        return handles;
    };

    extract_single_hook(
        scope,
        hooks_obj,
        hooks_meta.on_load.as_ref(),
        "onLoad",
        &mut handles,
    );
    extract_single_hook(
        scope,
        hooks_obj,
        hooks_meta.on_tool_call.as_ref(),
        "onToolCall",
        &mut handles,
    );
    extract_single_hook(
        scope,
        hooks_obj,
        hooks_meta.on_tool_result.as_ref(),
        "onToolResult",
        &mut handles,
    );
    extract_single_hook(
        scope,
        hooks_obj,
        hooks_meta.on_before_model.as_ref(),
        "onBeforeModel",
        &mut handles,
    );

    handles
}

/// Extract a single hook function from the hooks object, if declared.
fn extract_single_hook(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    hooks_obj: Local<Object>,
    declared: Option<&String>,
    hook_name: &str,
    handles: &mut HashMap<String, Global<v8::Function>>,
) {
    if declared.is_none() {
        return;
    }
    let Some(key) = v8::String::new(scope, hook_name) else {
        return;
    };
    let Some(val) = hooks_obj.get(scope, key.into()) else {
        return;
    };
    if val.is_function()
        && let Ok(fn_local) = v8::Local::<v8::Function>::try_from(val)
    {
        handles.insert(hook_name.to_string(), Global::new(scope, fn_local));
    }
}

/// Extract `handler` function handles for each command.
#[allow(clippy::cast_possible_truncation)]
fn extract_command_handles(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    manifest_obj: Local<Object>,
    commands: &[crate::manifest::LoadedCommand],
) -> Result<HashMap<String, Global<v8::Function>>, ExtensionError> {
    let mut handles = HashMap::new();
    if commands.is_empty() {
        return Ok(handles);
    }

    let cmds_key =
        v8::String::new(scope, "commands").expect("failed to create v8 string 'commands'");
    let Some(cmds_val) = manifest_obj.get(scope, cmds_key.into()) else {
        return Ok(handles);
    };
    let cmds_arr = v8::Local::<v8::Array>::try_from(cmds_val).map_err(|_| {
        ExtensionError::Manifest(manifest::ManifestError::InvalidField {
            field: "commands".to_string(),
            reason: "expected an array".to_string(),
        })
    })?;

    for (i, cmd) in commands.iter().enumerate() {
        let Some(element) = cmds_arr.get_index(scope, i as u32) else {
            return Err(ExtensionError::Manifest(
                manifest::ManifestError::InvalidField {
                    field: format!("commands[{i}].handler"),
                    reason: "command element missing".to_string(),
                },
            ));
        };
        let Ok(cmd_obj) = v8::Local::<Object>::try_from(element) else {
            return Err(ExtensionError::Manifest(
                manifest::ManifestError::InvalidField {
                    field: format!("commands[{i}]"),
                    reason: "expected an object".to_string(),
                },
            ));
        };
        let handler_key =
            v8::String::new(scope, "handler").expect("failed to create v8 string 'handler'");
        let Some(handler_val) = cmd_obj.get(scope, handler_key.into()) else {
            return Err(ExtensionError::CommandMissingHandler(cmd.name.clone()));
        };
        if !handler_val.is_function() {
            return Err(ExtensionError::CommandMissingHandler(cmd.name.clone()));
        }
        let handler_fn =
            v8::Local::<v8::Function>::try_from(handler_val).expect("checked is_function");
        handles.insert(cmd.name.clone(), Global::new(scope, handler_fn));
    }

    Ok(handles)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: spawn a runtime from an inline TypeScript source string.
    fn spawn_runtime(ts_source: &str) -> ExtensionRuntime {
        ExtensionRuntime::spawn(Url::parse("file:///test.ts").unwrap(), ts_source)
            .expect("spawn should succeed")
    }

    /// Helper: spawn from a temp dir with a main.ts file.
    fn spawn_from_dir(dir: &tempfile::TempDir, main_content: &str) -> ExtensionRuntime {
        let main_path = dir.path().join("main.ts");
        std::fs::write(&main_path, main_content).unwrap();
        ExtensionRuntime::spawn_from_file(&main_path, dir.path())
            .expect("spawn_from_file should succeed")
    }

    // =========================================================================
    // Manifest extraction tests
    // =========================================================================

    #[test]
    fn spawn_extracts_manifest_metadata() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "inline-test",
                version: "1.0.0",
            };
            "#,
        );

        let m = rt.manifest();
        assert_eq!(m.name, "inline-test");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert!(m.tools.is_empty());
        assert!(m.commands.is_empty());

        rt.shutdown().unwrap();
    }

    #[test]
    fn spawn_from_file_extracts_manifest_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = spawn_from_dir(
            &dir,
            r#"
            export default {
                name: "file-test",
                version: "2.5.0",
                tools: [{
                    name: "ping",
                    description: "Returns pong",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "pong",
                }],
            };
            "#,
        );

        let m = rt.manifest();
        assert_eq!(m.name, "file-test");
        assert_eq!(m.version.as_deref(), Some("2.5.0"));
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "ping");

        rt.shutdown().unwrap();
    }

    // =========================================================================
    // Tool call tests
    // =========================================================================

    #[tokio::test]
    async fn call_tool_executes_function() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "tool-test",
                tools: [{
                    name: "greet",
                    description: "Greet someone",
                    risk: "read" as const,
                    parameters: {},
                    execute: async (args: string) => {
                        const parsed = JSON.parse(args);
                        return `Hello, ${parsed.name}!`;
                    },
                }],
            };
            "#,
        );

        let result = rt.call_tool("greet", r#"{"name":"world"}"#).await.unwrap();
        assert_eq!(result, "Hello, world!");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_tool_unknown_returns_error() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "missing-tool-test",
                tools: [{
                    name: "exists",
                    description: "Exists",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
        );

        let err = rt.call_tool("nonexistent", "").await.unwrap_err();
        assert!(
            matches!(err, ExtensionError::ToolNotFound(ref name) if name == "nonexistent"),
            "expected ToolNotFound, got: {err}"
        );

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_tool_js_error_surfaces() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "error-test",
                tools: [{
                    name: "boom",
                    description: "Throws",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => { throw new Error("kaboom"); },
                }],
            };
            "#,
        );

        let err = rt.call_tool("boom", "").await.unwrap_err();
        assert!(
            matches!(err, ExtensionError::Execution(ref msg) if msg.contains("kaboom")),
            "expected Execution error with 'kaboom', got: {err}"
        );

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_tool_sequential_stateful() {
        let mut rt = spawn_runtime(
            r#"
            let counter = 0;
            export default {
                name: "counter-test",
                tools: [{
                    name: "next",
                    description: "Increment counter",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => {
                        counter += 1;
                        return String(counter);
                    },
                }],
            };
            "#,
        );

        assert_eq!(rt.call_tool("next", "").await.unwrap(), "1");
        assert_eq!(rt.call_tool("next", "").await.unwrap(), "2");
        assert_eq!(rt.call_tool("next", "").await.unwrap(), "3");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_tool_json_roundtrip() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "json-test",
                tools: [{
                    name: "echo",
                    description: "Echo JSON",
                    risk: "read" as const,
                    parameters: {},
                    execute: async (args: string) => {
                        const obj = JSON.parse(args);
                        return JSON.stringify({ got: obj });
                    },
                }],
            };
            "#,
        );

        let input = r#"{"name":"test","value":42}"#;
        let result = rt.call_tool("echo", input).await.unwrap();
        assert_eq!(result, r#"{"got":{"name":"test","value":42}}"#);

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn multiple_tools() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "multi-tool-test",
                tools: [
                    {
                        name: "upper",
                        description: "Uppercase",
                        risk: "read" as const,
                        parameters: {},
                        execute: async (args: string) => args.toUpperCase(),
                    },
                    {
                        name: "lower",
                        description: "Lowercase",
                        risk: "read" as const,
                        parameters: {},
                        execute: async (args: string) => args.toLowerCase(),
                    },
                ],
            };
            "#,
        );

        assert_eq!(rt.call_tool("upper", "hello").await.unwrap(), "HELLO");
        assert_eq!(rt.call_tool("lower", "WORLD").await.unwrap(), "world");

        rt.shutdown().unwrap();
    }

    // =========================================================================
    // Hook call tests
    // =========================================================================

    #[tokio::test]
    async fn call_hook_executes_function() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "hook-test",
                hooks: {
                    onLoad: async () => "loaded",
                    onToolCall: async (args: string) => `tool-called:${args}`,
                },
            };
            "#,
        );

        let result = rt.call_hook("onLoad", "").await.unwrap();
        assert_eq!(result, "loaded");

        let result = rt.call_hook("onToolCall", "greet").await.unwrap();
        assert_eq!(result, "tool-called:greet");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_hook_unknown_returns_error() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "hook-err-test",
                hooks: {
                    onLoad: async () => "ok",
                },
            };
            "#,
        );

        let err = rt.call_hook("onToolCall", "").await.unwrap_err();
        assert!(
            matches!(err, ExtensionError::HookNotFound(ref name) if name == "onToolCall"),
            "expected HookNotFound, got: {err}"
        );

        rt.shutdown().unwrap();
    }

    // =========================================================================
    // Command call tests
    // =========================================================================

    #[tokio::test]
    async fn call_command_executes_handler() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "cmd-test",
                commands: [{
                    name: "deploy",
                    description: "Deploy",
                    handler: async (args: string) => `deploying:${args}`,
                }],
            };
            "#,
        );

        let result = rt.call_command("deploy", "production").await.unwrap();
        assert_eq!(result, "deploying:production");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn call_command_unknown_returns_error() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "cmd-err-test",
                commands: [{
                    name: "exists",
                    description: "Exists",
                    handler: async () => "ok",
                }],
            };
            "#,
        );

        let err = rt.call_command("missing", "").await.unwrap_err();
        assert!(
            matches!(err, ExtensionError::CommandNotFound(ref name) if name == "missing"),
            "expected CommandNotFound, got: {err}"
        );

        rt.shutdown().unwrap();
    }

    // =========================================================================
    // Full manifest (tools + hooks + commands)
    // =========================================================================

    #[tokio::test]
    async fn full_manifest_with_all_features() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "full-test",
                version: "3.0.0",
                tools: [{
                    name: "search",
                    description: "Search for things",
                    risk: "read" as const,
                    parameters: {
                        query: { type: "string", description: "Search query", required: true },
                    },
                    execute: async (args: string) => {
                        const { query } = JSON.parse(args);
                        return JSON.stringify({ results: [query] });
                    },
                }],
                hooks: {
                    onLoad: async () => "extension-loaded",
                    onToolCall: async (args: string) => `before:${args}`,
                },
                commands: [{
                    name: "status",
                    description: "Show status",
                    handler: async () => "healthy",
                }],
            };
            "#,
        );

        // Verify manifest metadata
        let m = rt.manifest();
        assert_eq!(m.name, "full-test");
        assert_eq!(m.version.as_deref(), Some("3.0.0"));
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].name, "search");
        assert_eq!(m.tools[0].parameters.len(), 1);
        assert!(m.hooks.on_load.is_some());
        assert!(m.hooks.on_tool_call.is_some());
        assert!(m.hooks.on_tool_result.is_none());
        assert_eq!(m.commands.len(), 1);

        // Call tool
        let result = rt
            .call_tool("search", r#"{"query":"hello"}"#)
            .await
            .unwrap();
        assert_eq!(result, r#"{"results":["hello"]}"#);

        // Call hook
        let result = rt.call_hook("onLoad", "").await.unwrap();
        assert_eq!(result, "extension-loaded");

        // Call command
        let result = rt.call_command("status", "").await.unwrap();
        assert_eq!(result, "healthy");

        rt.shutdown().unwrap();
    }

    // =========================================================================
    // Lifecycle tests
    // =========================================================================

    #[tokio::test]
    async fn shutdown_terminates_cleanly() {
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "shutdown-test",
                tools: [{
                    name: "noop",
                    description: "No-op",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
        );

        // Verify it works before shutdown
        assert_eq!(rt.call_tool("noop", "").await.unwrap(), "ok");

        // Shutdown should succeed
        rt.shutdown().unwrap();

        // After shutdown, call should fail
        let err = rt.call_tool("noop", "").await.unwrap_err();
        assert!(matches!(err, ExtensionError::RuntimeShutdown));
    }

    #[tokio::test]
    async fn drop_cleans_up() {
        let rt = spawn_runtime(
            r#"
            export default {
                name: "drop-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "pong",
                }],
            };
            "#,
        );
        // Drop without explicit shutdown — should not hang or panic.
        drop(rt);
    }

    // =========================================================================
    // Multi-file (spawn_from_file) tests
    // =========================================================================

    #[tokio::test]
    async fn spawn_from_file_with_imports() {
        let dir = tempfile::tempdir().unwrap();

        // helper.ts
        std::fs::write(
            dir.path().join("helper.ts"),
            r#"export function add(a: number, b: number): number { return a + b; }"#,
        )
        .unwrap();

        // main.ts
        let mut rt = spawn_from_dir(
            &dir,
            r#"
            import { add } from "./helper.ts";
            export default {
                name: "import-test",
                tools: [{
                    name: "compute",
                    description: "Compute",
                    risk: "read" as const,
                    parameters: {},
                    execute: async (args: string) => {
                        return String(add(Number(args), 1));
                    },
                }],
            };
            "#,
        );

        let result = rt.call_tool("compute", "41").await.unwrap();
        assert_eq!(result, "42");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn spawn_from_file_nonexistent_entry_fails() {
        let dir = tempfile::tempdir().unwrap();
        let bad_path = dir.path().join("no_such_file.ts");

        let result = ExtensionRuntime::spawn_from_file(&bad_path, dir.path());
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

    #[tokio::test]
    async fn spawn_from_file_invalid_manifest_fails() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        // No default export
        std::fs::write(&main_path, r#"export function foo() { return 1; }"#).unwrap();

        let result = ExtensionRuntime::spawn_from_file(&main_path, dir.path());
        assert!(result.is_err(), "should fail for missing default export");
        match result {
            Err(ExtensionError::Manifest(_)) => {}
            Err(other) => panic!("expected Manifest error, got: {other}"),
            Ok(_) => panic!("should not succeed"),
        }
    }

    #[tokio::test]
    async fn spawn_from_file_tool_missing_execute_fails() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"
            export default {
                name: "no-execute-test",
                tools: [{
                    name: "broken",
                    description: "No execute",
                    risk: "read" as const,
                    parameters: {},
                }],
            };
            "#,
        )
        .unwrap();

        let result = ExtensionRuntime::spawn_from_file(&main_path, dir.path());
        assert!(result.is_err(), "should fail for tool without execute");
        match result {
            Err(ExtensionError::ToolMissingExecute(name)) => {
                assert_eq!(name, "broken");
            }
            Err(other) => panic!("expected ToolMissingExecute, got: {other}"),
            Ok(_) => panic!("should not succeed"),
        }
    }

    #[tokio::test]
    async fn spawn_from_file_command_missing_handler_fails() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(
            &main_path,
            r#"
            export default {
                name: "no-handler-test",
                tools: [{
                    name: "ok",
                    description: "OK",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
                commands: [{
                    name: "broken",
                    description: "No handler",
                }],
            };
            "#,
        )
        .unwrap();

        let result = ExtensionRuntime::spawn_from_file(&main_path, dir.path());
        assert!(result.is_err(), "should fail for command without handler");
        match result {
            Err(ExtensionError::CommandMissingHandler(name)) => {
                assert_eq!(name, "broken");
            }
            Err(other) => panic!("expected CommandMissingHandler, got: {other}"),
            Ok(_) => panic!("should not succeed"),
        }
    }

    // =========================================================================
    // Host function integration
    // =========================================================================

    #[tokio::test]
    async fn rho_global_available_in_extension() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = spawn_from_dir(
            &dir,
            r#"
            export default {
                name: "rho-test",
                tools: [
                    {
                        name: "getCwd",
                        description: "Get CWD",
                        risk: "read" as const,
                        parameters: {},
                        execute: async () => rho.getCwd(),
                    },
                    {
                        name: "logSomething",
                        description: "Log something",
                        risk: "read" as const,
                        parameters: {},
                        execute: async () => {
                            rho.log("info", "test log from extension");
                            return "logged";
                        },
                    },
                ],
            };
            "#,
        );

        // getCwd should return the extension's root dir
        let cwd = rt.call_tool("getCwd", "").await.unwrap();
        assert_eq!(cwd, dir.path().to_str().unwrap());

        // logSomething should not crash and return "logged"
        let result = rt.call_tool("logSomething", "").await.unwrap();
        assert_eq!(result, "logged");

        rt.shutdown().unwrap();
    }

    #[tokio::test]
    async fn rho_global_falls_back_without_host_state() {
        // ExtensionRuntime::spawn (inline) does NOT inject HostState,
        // but rho global is still available (getCwd falls back to env cwd).
        let mut rt = spawn_runtime(
            r#"
            export default {
                name: "rho-fallback-test",
                tools: [{
                    name: "getCwd",
                    description: "Get CWD",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => rho.getCwd(),
                }],
            };
            "#,
        );

        let cwd = rt.call_tool("getCwd", "").await.unwrap();
        assert!(!cwd.is_empty(), "should return a non-empty path");

        rt.shutdown().unwrap();
    }
}
