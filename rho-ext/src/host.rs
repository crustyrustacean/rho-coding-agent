//! Host functions exposed to extension JS via `rho.*` global.
//!
//! This module defines `deno_core` ops (`#[op2]`) and bundles them into a
//! `deno_core::Extension` named `"rho_host"`. An `OpState` cell
//! (`HostState`) is injected at runtime creation time so that ops can
//! access the extension's working directory and other shared context.
//!
//! Implemented host functions:
//!
//! | JS call | Rust op | Behaviour |
//! |---|---|---|
//! | `rho.log(level, msg)` | `op_rho_log` | Emit a structured log via `tracing` |
//! | `rho.getCwd()` | `op_rho_get_cwd` | Return the extension's working directory |
//! | `rho.readFile(path)` | `op_rho_read_file` | Read a file within sandbox |
//! | `rho.writeFile(path, content)` | `op_rho_write_file` | Write a file within sandbox |
//! | `rho.runCommand(cmd, args)` | `op_rho_run_command` | Run a shell command (if permitted) |
//! | `rho.getModel()` | `op_rho_get_model` | Return the currently active model name |

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use deno_core::{OpState, op2};

/// Maximum file size that `readFile` will return (1 MiB).
const MAX_READ_BYTES: u64 = 1024 * 1024;

// ── OpState cell ──────────────────────────────────────────────────────────────

/// Shared state available to all rho host ops.
///
/// Inserted into `OpState` via `state.put()` when the extension is
/// initialised. Individual ops borrow it via `state.borrow::<HostState>()`.
#[derive(Debug)]
pub struct HostState {
    /// The working directory rho was started in (or the extension's root dir).
    pub cwd: PathBuf,
    /// Canonicalised directory roots the extension may read/write within.
    ///
    /// Defaults to just `[cwd]` if empty. Paths are canonicalised at
    /// construction time so that sandbox checks can use a simple
    /// `starts_with` comparison.
    pub allowed_paths: Vec<PathBuf>,
    /// Whether the extension is allowed to run shell commands via `rho.runCommand`.
    ///
    /// Defaults to `false`. Must be explicitly enabled via the `commands = true`
    /// permission in the extension config.
    pub allow_commands: bool,
    /// The name of the currently active model.
    ///
    /// Shared via `Arc<Mutex<String>>` so that the agent loop can update it
    /// when the model changes (e.g. via `/model` command) and extensions
    /// always see the current value via `rho.getModel()`.
    ///
    /// Defaults to an empty string if not set.
    pub model: Arc<Mutex<String>>,
}

impl HostState {
    /// Create a new `HostState` with the given cwd and optional extra
    /// allowed paths.
    ///
    /// The `cwd` itself is always added to the allowed set. Additional
    /// paths in `extra_allowed` are canonicalised; non-existent paths
    /// are silently skipped.
    pub fn new(cwd: PathBuf, extra_allowed: Vec<PathBuf>) -> Self {
        Self::new_with_permissions(cwd, extra_allowed, false)
    }

    /// Create a new `HostState` with full permission control.
    ///
    /// See [`HostState::new`] for path handling details.
    pub fn new_with_permissions(
        cwd: PathBuf,
        extra_allowed: Vec<PathBuf>,
        allow_commands: bool,
    ) -> Self {
        Self::new_with_model(cwd, extra_allowed, allow_commands, String::new())
    }

    /// Create a new `HostState` with full control, including the model name.
    ///
    /// See [`HostState::new`] for path handling details.
    pub fn new_with_model(
        cwd: PathBuf,
        extra_allowed: Vec<PathBuf>,
        allow_commands: bool,
        model: String,
    ) -> Self {
        let mut allowed = Vec::new();

        // Always include cwd (canonicalise if possible, use as-is if not)
        if let Ok(canonical) = cwd.canonicalize() {
            allowed.push(canonical);
        } else {
            allowed.push(cwd.clone());
        }

        // Add extra allowed paths
        for p in extra_allowed {
            if let Ok(canonical) = p.canonicalize() {
                if !allowed.contains(&canonical) {
                    allowed.push(canonical);
                }
            }
            // Non-existent paths are silently skipped
        }

        Self {
            cwd,
            allowed_paths: allowed,
            allow_commands,
            model: Arc::new(Mutex::new(model)),
        }
    }

    /// Builder: set the model name.
    #[must_use]
    pub fn with_model(self, model: impl Into<String>) -> Self {
        *self.model.lock().expect("HostState model lock poisoned") = model.into();
        self
    }

    /// Builder: enable or disable command execution permission.
    #[must_use]
    pub fn with_commands(mut self, allow: bool) -> Self {
        self.allow_commands = allow;
        self
    }

    /// Resolve a path relative to cwd and sandbox-check it.
    ///
    /// Relative paths are resolved against cwd. The resulting canonical
    /// path must start with one of `allowed_paths`.
    ///
    /// Returns the canonical absolute path on success.
    fn resolve_and_check(&self, path: &str) -> Result<PathBuf, String> {
        let requested = Path::new(path);

        // Resolve relative paths against cwd
        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.cwd.join(requested)
        };

        // Canonicalise (resolves symlinks, requires file to exist)
        let canonical = absolute.canonicalize().map_err(|e| {
            format!(
                "rho host: cannot resolve path '{}': {e}",
                absolute.display()
            )
        })?;

        // Sandbox check: canonical path must be within an allowed root
        let is_allowed = self.allowed_paths.iter().any(|root| canonical.starts_with(root));
        if !is_allowed {
            return Err(format!(
                "rho host: path '{}' is outside the extension sandbox",
                canonical.display()
            ));
        }

        Ok(canonical)
    }

    /// Return a shared handle to the model name.
    ///
    /// The returned `Arc<Mutex<String>>` can be cloned and held by the
    /// [`ExtensionRuntime`](crate::runtime::ExtensionRuntime) so that the
    /// agent loop can update the model name from outside the V8 thread.
    pub fn model_handle(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.model)
    }

    /// Sandbox-check a write target (file may not exist yet).
    ///
    /// For writes, the file and some parent directories might not exist.
    /// We walk up the path until we find an existing directory, canonicalise
    /// that, and verify it's within the sandbox.
    fn resolve_and_check_write(&self, path: &str) -> Result<PathBuf, String> {
        let requested = Path::new(path);

        // Resolve relative paths against cwd
        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.cwd.join(requested)
        };

        // Walk up to find the first existing ancestor directory.
        // canonicalise that, verify it's in the sandbox.
        let mut check_dir = absolute.parent().unwrap_or(Path::new("."));
        let canonical_ancestor = loop {
            match check_dir.canonicalize() {
                Ok(c) => break c,
                Err(_) => {
                    match check_dir.parent() {
                        Some(parent) => check_dir = parent,
                        None => {
                            return Err(format!(
                                "rho host: no existing ancestor directory for '{}'",
                                absolute.display()
                            ));
                        }
                    }
                }
            }
        };

        let is_allowed = self.allowed_paths.iter().any(|root| canonical_ancestor.starts_with(root));
        if !is_allowed {
            return Err(format!(
                "rho host: path '{}' is outside the extension sandbox",
                absolute.display()
            ));
        }

        Ok(absolute)
    }
}

// ── Ops ───────────────────────────────────────────────────────────────────────

/// `rho.log(level, message)` — emit a structured log line.
///
/// `level` must be one of `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`.
/// Invalid levels are silently promoted to `"info"`.
#[op2(fast)]
pub fn op_rho_log(#[string] level: &str, #[string] message: &str) {
    match level {
        "trace" => tracing::trace!(target: "rho::ext", "{}", message),
        "debug" => tracing::debug!(target: "rho::ext", "{}", message),
        "info" => tracing::info!(target: "rho::ext", "{}", message),
        "warn" => tracing::warn!(target: "rho::ext", "{}", message),
        "error" => tracing::error!(target: "rho::ext", "{}", message),
        _ => tracing::info!(target: "rho::ext", "[{}] {}", level, message),
    }
}

/// `rho.getCwd()` — return the extension's working directory as a string.
#[op2]
#[string]
pub fn op_rho_get_cwd(state: &mut OpState) -> String {
    let cwd = state.try_borrow::<HostState>().map_or_else(
        || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        |s| s.cwd.clone(),
    );

    cwd.to_str()
        .map_or_else(|| ".".to_string(), std::string::ToString::to_string)
}

/// `rho.getModel()` — return the name of the currently active model.
///
/// The model name is shared state that can be updated from outside the V8
/// thread (e.g. when the user switches models). Extensions always see the
/// current value.
///
/// Returns an empty string if no model has been set.
#[op2]
#[string]
pub fn op_rho_get_model(state: &mut OpState) -> String {
    state
        .try_borrow::<HostState>()
        .map_or_else(String::new, |h| {
            h.model
                .lock()
                .expect("HostState model lock poisoned")
                .clone()
        })
}

/// `rho.readFile(path)` — read a file within the extension sandbox.
///
/// The path is resolved relative to the extension's cwd. The canonical
/// path must be within one of `allowed_paths`. Files larger than 1 MiB
/// are rejected.
///
/// Returns the file contents as a string (UTF-8), or an error string
/// starting with `__ERROR__`.
#[op2]
#[string]
pub fn op_rho_read_file(state: &mut OpState, #[string] path: String) -> String {
    let canonical = {
        let host = state.borrow::<HostState>();
        match host.resolve_and_check(&path) {
            Ok(p) => p,
            Err(e) => return format!("__ERROR__{e}"),
        }
    };

    // Check file size
    match std::fs::metadata(&canonical) {
        Ok(meta) => {
            if meta.len() > MAX_READ_BYTES {
                return format!(
                    "__ERROR__rho.readFile: file '{}' is too large ({} bytes, max {MAX_READ_BYTES})",
                    canonical.display(),
                    meta.len()
                );
            }
        }
        Err(e) => {
            return format!(
                "__ERROR__rho.readFile: cannot stat '{}': {e}",
                canonical.display()
            );
        }
    }

    // Read file
    match std::fs::read_to_string(&canonical) {
        Ok(content) => content,
        Err(e) => {
            format!(
                "__ERROR__rho.readFile: cannot read '{}': {e}",
                canonical.display()
            )
        }
    }
}

/// `rho.writeFile(path, content)` — write a file within the extension sandbox.
///
/// The path is resolved relative to the extension's cwd. The parent
/// directory's canonical path must be within one of `allowed_paths`.
/// Parent directories are created if they don't exist.
///
/// Returns `"ok"` on success, or an error string starting with `__ERROR__`.
#[op2]
#[string]
pub fn op_rho_write_file(state: &mut OpState, #[string] path: String, #[string] content: String) -> String {
    let absolute = {
        let host = state.borrow::<HostState>();
        match host.resolve_and_check_write(&path) {
            Ok(p) => p,
            Err(e) => return format!("__ERROR__{e}"),
        }
    };

    // Create parent directories if needed
    if let Some(parent) = absolute.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!(
                "__ERROR__rho.writeFile: cannot create parent directory '{}': {e}",
                parent.display()
            );
        }
    }

    // Write file
    match std::fs::write(&absolute, &content) {
        Ok(()) => "ok".to_string(),
        Err(e) => {
            format!(
                "__ERROR__rho.writeFile: cannot write '{}': {e}",
                absolute.display()
            )
        }
    }
}

/// `rho.runCommand(cmd, args)` — run a shell command and return its output.
///
/// Requires the `commands` permission to be enabled in the extension config.
/// If the extension does not have this permission, an error is returned.
///
/// `cmd` is the command to execute (e.g. `"git"`, `"npm"`).
/// `args_json` is a JSON-encoded array of string arguments, or an empty string
/// for no arguments.
///
/// Returns a JSON object `{ "stdout": "...", "stderr": "...", "exitCode": N }`,
/// or an error string starting with `__ERROR__`.
#[op2]
#[string]
pub fn op_rho_run_command(
    state: &mut OpState,
    #[string] cmd: String,
    #[string] args_json: String,
) -> String {
    // 1. Permission check
    let allow = state
        .try_borrow::<HostState>()
        .map_or(false, |h| h.allow_commands);

    if !allow {
        return format!(
            "__ERROR__rho.runCommand: extension does not have command execution permission \
             (enable with `commands = true` in config)"
        );
    }

    // 2. Parse args
    let args: Vec<String> = if args_json.is_empty() {
        vec![]
    } else {
        match serde_json::from_str(&args_json) {
            Ok(a) => a,
            Err(e) => {
                return format!("__ERROR__rho.runCommand: invalid args JSON: {e}");
            }
        }
    };

    // 3. Execute command
    match std::process::Command::new(&cmd).args(&args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);

            // Build JSON result
            match serde_json::to_string(&serde_json::json!({
                "stdout": stdout,
                "stderr": stderr,
                "exitCode": exit_code,
            })) {
                Ok(json) => json,
                Err(e) => format!("__ERROR__rho.runCommand: failed to serialise result: {e}"),
            }
        }
        Err(e) => {
            format!("__ERROR__rho.runCommand: failed to execute '{cmd}': {e}")
        }
    }
}

// ── Extension definition ─────────────────────────────────────────────────────

// The `rho_host` deno_core extension.
//
// Registers the ops above and provides an ESM shim that creates the `rho`
// global object on `globalThis`.
//
// Usage: `rho_host::init()` → add to `RuntimeOptions.extensions`.
deno_core::extension!(
    rho_host,
    ops = [op_rho_log, op_rho_get_cwd, op_rho_get_model, op_rho_read_file, op_rho_write_file, op_rho_run_command],
    esm_entry_point = "ext:rho_host/host_shim.js",
    esm = [ dir "src", "host_shim.js" ],
);

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::JsRuntime;

    /// Helper: create a JsRuntime with the rho_host extension + HostState.
    fn runtime_with_state(cwd: &str) -> JsRuntime {
        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        // Inject HostState
        rt.op_state().borrow_mut().put(HostState::new(
            PathBuf::from(cwd),
            vec![],
        ));

        rt
    }

    /// Helper: create a JsRuntime with the rho_host extension but no HostState.
    fn runtime_without_state() -> JsRuntime {
        JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        })
    }

    /// Evaluate a synchronous expression and return the string result.
    fn eval(rt: &mut JsRuntime, code: &str) -> String {
        let result = rt
            .execute_script(
                deno_core::FastString::from_static("<test>"),
                code.to_string(),
            )
            .expect("execute_script should not fail");

        // Run event loop to completion
        let local = tokio::task::LocalSet::new();
        local
            .block_on(
                &tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
                rt.run_event_loop(deno_core::PollEventLoopOptions::default()),
            )
            .unwrap();

        // Extract the string value
        deno_core::scope!(scope, rt);
        let local = deno_core::v8::Local::new(scope, result);
        local.to_rust_string_lossy(scope)
    }

    /// Evaluate an expression that may throw a JS error.
    ///
    /// Returns `Err(message)` if the script threw, `Ok(value)` otherwise.
    fn eval_or_error(rt: &mut JsRuntime, code: &str) -> Result<String, String> {
        match rt.execute_script(
            deno_core::FastString::from_static("<test>"),
            code.to_string(),
        ) {
            Ok(result) => {
                // Run event loop to completion
                let local = tokio::task::LocalSet::new();
                local
                    .block_on(
                        &tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap(),
                        rt.run_event_loop(deno_core::PollEventLoopOptions::default()),
                    )
                    .unwrap();

                deno_core::scope!(scope, rt);
                let local = deno_core::v8::Local::new(scope, result);
                Ok(local.to_rust_string_lossy(scope))
            }
            Err(e) => {
                // Also run the event loop to clean up
                let local = tokio::task::LocalSet::new();
                let _ = local.block_on(
                    &tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap(),
                    rt.run_event_loop(deno_core::PollEventLoopOptions::default()),
                );
                Err(format!("{e}"))
            }
        }
    }

    #[test]
    fn rho_log_does_not_crash() {
        let mut rt = runtime_with_state("/tmp");
        // Should not panic — just logs via tracing (invisible in test)
        eval(&mut rt, r#"rho.log("info", "hello from test")"#);
    }

    #[test]
    fn rho_log_accepts_all_levels() {
        let mut rt = runtime_with_state("/tmp");
        for level in &["trace", "debug", "info", "warn", "error"] {
            eval(&mut rt, &format!(r#"rho.log("{}", "msg")"#, level));
        }
    }

    #[test]
    fn rho_log_accepts_unknown_level() {
        let mut rt = runtime_with_state("/tmp");
        // Unknown level should not crash — falls through to default
        eval(&mut rt, r#"rho.log("custom", "msg")"#);
    }

    #[test]
    fn rho_get_cwd_returns_injected_state() {
        let mut rt = runtime_with_state("/my/custom/dir");
        let result = eval(&mut rt, r#"rho.getCwd()"#);
        assert_eq!(result, "/my/custom/dir");
    }

    #[test]
    fn rho_get_cwd_falls_back_without_state() {
        let mut rt = runtime_without_state();
        let result = eval(&mut rt, r#"rho.getCwd()"#);
        // Should return some valid path string (current dir)
        assert!(
            result.starts_with("/"),
            "expected absolute path, got: {}",
            result
        );
    }

    #[test]
    fn rho_global_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r#"typeof rho"#);
        assert_eq!(result, "object");
    }

    #[test]
    fn rho_global_has_expected_methods() {
        let mut rt = runtime_with_state("/tmp");
        for method in &["log", "getCwd", "getModel", "readFile", "writeFile", "runCommand"] {
            let result = eval(&mut rt, &format!(r#"typeof rho.{}"#, method));
            assert_eq!(result, "function", "rho.{} should be a function", method);
        }
    }

    // ── readFile tests ──────────────────────────────────────────────────────

    #[test]
    fn read_file_reads_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let result = eval(&mut rt, r#"rho.readFile("hello.txt")"#);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn read_file_reads_absolute_path_within_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.json");
        std::fs::write(&file_path, r#"{"key":"value"}"#).unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(r#"rho.readFile('{}')"#, file_path.display());
        let result = eval(&mut rt, &code);
        assert_eq!(result, r#"{"key":"value"}"#);
    }

    #[test]
    fn read_file_reads_subdirectory_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/nested.txt"), "nested content").unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let result = eval(&mut rt, r#"rho.readFile("sub/nested.txt")"#);
        assert_eq!(result, "nested content");
    }

    #[test]
    fn read_file_nonexistent_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let result = eval_or_error(&mut rt, r#"rho.readFile("no_such_file.txt")"#);
        let err = result.unwrap_err();
        assert!(err.contains("cannot resolve"), "expected error, got: {err}");
    }

    #[test]
    fn read_file_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        // Write a file outside the sandbox
        let outside_dir = tempfile::tempdir().unwrap();
        std::fs::write(outside_dir.path().join("secret.txt"), "secret").unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(r#"rho.readFile('{}')"#, outside_dir.path().join("secret.txt").display());
        let result = eval_or_error(&mut rt, &code);
        let err = result.unwrap_err();
        assert!(err.contains("outside"), "expected sandbox error, got: {err}");
    }

    #[test]
    fn read_file_dotdot_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir_all(&subdir).unwrap();

        // Try to read ../../etc/passwd relative to the sandbox
        let mut rt = runtime_with_state(subdir.to_str().unwrap());
        let result = eval_or_error(&mut rt, r#"rho.readFile("../../etc/passwd")"#);
        let err = result.unwrap_err();
        // May hit "outside sandbox" or "cannot resolve" — both are fine
        assert!(err.contains("outside") || err.contains("cannot resolve"), "expected sandbox error, got: {err}");
    }

    #[test]
    fn read_file_with_extra_allowed_path() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        std::fs::write(dir2.path().join("extra.txt"), "extra content").unwrap();

        // Create runtime with dir1 as cwd but dir2 as extra allowed
        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });
        rt.op_state().borrow_mut().put(HostState::new(
            dir1.path().to_path_buf(),
            vec![dir2.path().to_path_buf()],
        ));

        let code = format!(r#"rho.readFile('{}')"#, dir2.path().join("extra.txt").display());
        let result = eval(&mut rt, &code);
        assert_eq!(result, "extra content");
    }

    // ── writeFile tests ─────────────────────────────────────────────────────

    #[test]
    fn write_file_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        // writeFile returns undefined (void), just check no error
        eval(&mut rt, r#"rho.writeFile("output.txt", "hello from write")"#);

        let written = std::fs::read_to_string(dir.path().join("output.txt")).unwrap();
        assert_eq!(written, "hello from write");
    }

    #[test]
    fn write_file_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.txt"), "old content").unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        eval(&mut rt, r#"rho.writeFile("data.txt", "new content")"#);

        let written = std::fs::read_to_string(dir.path().join("data.txt")).unwrap();
        assert_eq!(written, "new content");
    }

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        eval(&mut rt, r#"rho.writeFile("deep/nested/dir/file.txt", "deep content")"#);

        let written = std::fs::read_to_string(dir.path().join("deep/nested/dir/file.txt")).unwrap();
        assert_eq!(written, "deep content");
    }

    #[test]
    fn write_file_absolute_path_within_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("abs.txt");

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(r#"rho.writeFile('{}', "abs content")"#, file_path.display());
        eval(&mut rt, &code);

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "abs content");
    }

    #[test]
    fn write_file_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(r#"rho.writeFile('{}/evil.txt', "pwned")"#, outside_dir.path().display());
        let result = eval_or_error(&mut rt, &code);
        let err = result.unwrap_err();
        assert!(err.contains("outside"), "expected sandbox error, got: {err}");
    }

    // ── round-trip test ─────────────────────────────────────────────────────

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());

        // Write
        eval(&mut rt, r#"rho.writeFile("roundtrip.json", JSON.stringify({a:1,b:2}))"#);

        // Read back
        let result = eval(&mut rt, r#"rho.readFile("roundtrip.json")"#);
        assert_eq!(result, r#"{"a":1,"b":2}"#);
    }

    // ── runCommand tests ────────────────────────────────────────────────────

    /// Helper: create a JsRuntime with command execution enabled.
    fn runtime_with_commands(cwd: &str) -> JsRuntime {
        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        rt.op_state().borrow_mut().put(
            HostState::new_with_permissions(
                PathBuf::from(cwd),
                vec![],
                true, // allow_commands
            ),
        );

        rt
    }

    #[test]
    fn run_command_echo() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        let result = eval(
            &mut rt,
            r#"JSON.stringify(rho.runCommand("echo", ["hello world"]))"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 0);
        assert!(parsed["stdout"].as_str().unwrap().contains("hello world"));
    }

    #[test]
    fn run_command_no_args() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        let result = eval(
            &mut rt,
            r#"JSON.stringify(rho.runCommand("echo"))"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 0);
    }

    #[test]
    fn run_command_captures_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        // Use a command that writes to stderr: `echo msg >&2` in bash
        let result = eval(
            &mut rt,
            r#"JSON.stringify(rho.runCommand("bash", ["-c", "echo error >&2"]))"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 0);
        assert!(parsed["stderr"].as_str().unwrap().contains("error"));
    }

    #[test]
    fn run_command_nonzero_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        let result = eval(
            &mut rt,
            r#"JSON.stringify(rho.runCommand("bash", ["-c", "exit 42"]))"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 42);
    }

    #[test]
    fn run_command_nonexistent_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        let result = eval_or_error(
            &mut rt,
            r#"rho.runCommand("no_such_binary_xyz_123")"#,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("failed to execute"),
            "expected execution error, got: {err}"
        );
    }

    #[test]
    fn run_command_blocked_without_permission() {
        let dir = tempfile::tempdir().unwrap();
        // Default runtime (no commands permission)
        let mut rt = runtime_with_state(dir.path().to_str().unwrap());

        let result = eval_or_error(
            &mut rt,
            r#"rho.runCommand("echo", ["hello"])"#,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("command execution permission"),
            "expected permission error, got: {err}"
        );
    }

    #[test]
    fn run_command_blocked_without_host_state() {
        let mut rt = runtime_without_state();

        let result = eval_or_error(
            &mut rt,
            r#"rho.runCommand("echo", ["hello"])"#,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("command execution permission"),
            "expected permission error, got: {err}"
        );
    }

    // ── getModel tests ──────────────────────────────────────────────────────

    #[test]
    fn get_model_returns_model_from_host_state() {
        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        rt.op_state().borrow_mut().put(
            HostState::new_with_model(
                PathBuf::from("/tmp"),
                vec![],
                false,
                "claude-sonnet-4-20250514".to_string(),
            ),
        );

        let result = eval(&mut rt, r#"rho.getModel()"#);
        assert_eq!(result, "claude-sonnet-4-20250514");
    }

    #[test]
    fn get_model_returns_empty_without_host_state() {
        let mut rt = runtime_without_state();
        let result = eval(&mut rt, r#"rho.getModel()"#);
        assert_eq!(result, "");
    }

    #[test]
    fn get_model_returns_empty_by_default() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r#"rho.getModel()"#);
        assert_eq!(result, "");
    }

    #[test]
    fn get_model_reflects_live_update() {
        let model: Arc<Mutex<String>> = Arc::new(Mutex::new("gpt-4o".to_string()));
        let model_clone = Arc::clone(&model);

        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        // Build HostState with the shared model handle
        let host_state = HostState::new_with_model(
            PathBuf::from("/tmp"),
            vec![],
            false,
            "gpt-4o".to_string(),
        );
        // We need to replace the model Arc with our shared one
        {
            let op_state = rt.op_state();
            op_state.borrow_mut().put(HostState {
                cwd: PathBuf::from("/tmp"),
                allowed_paths: host_state.allowed_paths.clone(),
                allow_commands: false,
                model: model_clone,
            });
        }

        // Initial value
        let result = eval(&mut rt, r#"rho.getModel()"#);
        assert_eq!(result, "gpt-4o");

        // Update from outside
        *model.lock().unwrap() = "claude-sonnet-4-20250514".to_string();

        // Extension should see the new value
        let result = eval(&mut rt, r#"rho.getModel()"#);
        assert_eq!(result, "claude-sonnet-4-20250514");
    }
}
