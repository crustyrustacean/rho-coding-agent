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
//! | `rho.getCwd()` | `op_rho_get_cwd` | Return the extension's working directory (project root) |
//! | `rho.getProjectRoot()` | `op_rho_get_project_root` | Return the project root path |
//! | `rho.readFile(path)` | `op_rho_read_file` | Read a file within sandbox |
//! | `rho.writeFile(path, content)` | `op_rho_write_file` | Write a file within sandbox |
//! | `rho.runCommand(cmd, args)` | `op_rho_run_command` | Run a shell command (if permitted) |
//! | `rho.getModel()` | `op_rho_get_model` | Return the currently active model name |
//! | `rho.fetchUrl(opts)` | `op_rho_fetch_url` | Make an HTTP request (if permitted) |

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use deno_core::{OpState, op2};
use url::Url;

// Import Phase 3 boilerplate-reduction macros.
use crate::{err, json, require_field, require_perm};
use rho_core::denylist::CommandDenylist;

/// Maximum file size that `readFile` will return (1 MiB).
const MAX_READ_BYTES: u64 = 1024 * 1024;

// ── OpState cell ──────────────────────────────────────────────────────────────

/// Shared state available to all rho host ops.
///
/// Inserted into `OpState` via `state.put()` when the extension is
/// initialised. Individual ops borrow it via `state.borrow::<HostState>()`.
#[derive(Debug)]
pub struct HostState {
    /// The project root directory (sandbox root).
    ///
    /// This is used as the working directory for relative path resolution
    /// in `rho.readFile`, `rho.writeFile`, etc. All relative paths are
    /// joined to this directory before sandbox validation.
    pub cwd: PathBuf,
    /// The project root path, stored explicitly for `rho.getProjectRoot()`.
    ///
    /// Always equals `cwd` but named separately for clarity in the host API.
    pub project_root: PathBuf,
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
    /// Whether the extension is allowed to make HTTP requests via `rho.fetchUrl`.
    /// Defaults to `false`. Must be explicitly enabled via the `network = true`
    /// permission in the extension config.
    pub allow_network: bool,
    /// Command denylist applied to `rho.runCommand()` calls.
    ///
    /// Same denylist as the built-in `RunCommand` tool — blocks destructive
    /// commands, network egress tools, and dangerous flag combinations.
    pub denylist: CommandDenylist,
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
    /// Create a new `HostState`.
    ///
    /// The `project_root` becomes `cwd` (the working directory for relative
    /// path resolution) and is added to `allowed_paths`. The `ext_root_dir`
    /// (the extension's own directory) is also added to `allowed_paths` so
    /// extensions can access their own data files. Additional paths in
    /// `extra_allowed` are resolved relative to `project_root` and added;
    /// non-existent paths are silently skipped.
    pub fn new(
        project_root: PathBuf,
        ext_root_dir: &Path,
        extra_allowed: Vec<PathBuf>,
        denylist: CommandDenylist,
    ) -> Self {
        let mut allowed = Vec::new();

        // Always include the project root (canonicalise if possible).
        if let Ok(canonical) = project_root.canonicalize() {
            allowed.push(canonical);
        } else {
            allowed.push(project_root.clone());
        }

        // Add the extension's own directory so extensions can access
        // their own data files (via absolute paths or rho.getProjectRoot()).
        let ext_root = ext_root_dir.to_path_buf();
        if let Ok(canonical) = ext_root_dir.canonicalize() {
            if !allowed.contains(&canonical) {
                allowed.push(canonical);
            }
        } else if !allowed.contains(&ext_root) {
            allowed.push(ext_root);
        }

        // Add extra allowed paths (resolved relative to project root).
        for p in extra_allowed {
            let resolved = project_root.join(p);
            if let Ok(canonical) = resolved.canonicalize()
                && !allowed.contains(&canonical)
            {
                allowed.push(canonical);
            }
        }

        Self {
            cwd: project_root.clone(),
            project_root,
            allowed_paths: allowed,
            allow_commands: false,
            allow_network: false,
            denylist,
            model: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Builder: set the model name.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn with_model(self, model: impl Into<String>) -> Self {
        *self.model.lock().expect("HostState model lock poisoned") = model.into();
        self
    }

    /// Builder: enable or disable network access.
    #[must_use]
    pub fn with_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
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
        let is_allowed = self
            .allowed_paths
            .iter()
            .any(|root| canonical.starts_with(root));
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
                Err(_) => match check_dir.parent() {
                    Some(parent) => check_dir = parent,
                    None => {
                        return Err(format!(
                            "rho host: no existing ancestor directory for '{}'",
                            absolute.display()
                        ));
                    }
                },
            }
        };

        let is_allowed = self
            .allowed_paths
            .iter()
            .any(|root| canonical_ancestor.starts_with(root));
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
///
/// This returns the project root (sandbox root). All relative file paths
/// passed to `rho.readFile` and `rho.writeFile` are resolved from this directory.
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

/// `rho.getProjectRoot()` — return the project root path.
///
/// Returns the same path as `rho.getCwd()` but with a more explicit name.
/// Useful for extensions that need to construct absolute paths to project
/// files (e.g. `rho.getProjectRoot() + "/.rho/extensions/my-ext/data.json"`).
#[op2]
#[string]
pub fn op_rho_get_project_root(state: &mut OpState) -> String {
    state.try_borrow::<HostState>().map_or_else(
        || {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .to_string()
        },
        |s| s.project_root.to_string_lossy().to_string(),
    )
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
pub fn op_rho_read_file(state: &mut OpState, #[string] path: &str) -> String {
    let canonical = {
        let host = state.borrow::<HostState>();
        match host.resolve_and_check(path) {
            Ok(p) => p,
            Err(e) => return err!("{e}"),
        }
    };

    // Check file size
    match std::fs::metadata(&canonical) {
        Ok(meta) if meta.len() > MAX_READ_BYTES => {
            return err!(
                "rho.readFile: file '{}' is too large ({} bytes, max {MAX_READ_BYTES})",
                canonical.display(),
                meta.len()
            );
        }
        Err(e) => {
            return err!("rho.readFile: cannot stat '{}': {e}", canonical.display());
        }
        _ => {}
    }

    // Read file
    match std::fs::read_to_string(&canonical) {
        Ok(content) => content,
        Err(e) => err!("rho.readFile: cannot read '{}': {e}", canonical.display()),
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
pub fn op_rho_write_file(
    state: &mut OpState,
    #[string] path: &str,
    #[string] content: &str,
) -> String {
    let absolute = {
        let host = state.borrow::<HostState>();
        match host.resolve_and_check_write(path) {
            Ok(p) => p,
            Err(e) => return err!("{e}"),
        }
    };

    // Create parent directories if needed
    if let Some(parent) = absolute.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return err!(
            "rho.writeFile: cannot create parent directory '{}': {e}",
            parent.display()
        );
    }

    // Write file
    match std::fs::write(&absolute, content) {
        Ok(()) => "ok".to_string(),
        Err(e) => err!("rho.writeFile: cannot write '{}': {e}", absolute.display()),
    }
}

/// `rho.runCommand(cmd, args)` — run a shell command and return its output.
///
/// Requires the `commands` permission to be enabled in the extension config.
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
    #[string] cmd: &str,
    #[string] args_json: &str,
) -> String {
    require_perm!(state, allow_commands, "command execution");

    // Build full command string for denylist checking.
    let full_command = if args_json.is_empty() {
        cmd.to_owned()
    } else {
        let args: Vec<String> = match serde_json::from_str(args_json) {
            Ok(a) => a,
            Err(e) => return err!("invalid args JSON: {e}"),
        };
        let mut parts = vec![cmd.to_owned()];
        parts.extend(args);
        parts.join(" ")
    };

    // Denylist check — refuse dangerous commands before execution.
    // Extract cwd for use as the command's working directory.
    let cwd = {
        let state_ref = state.borrow::<HostState>();
        if let Some(reason) = state_ref.denylist.check(&full_command) {
            return err!("command denied: {reason}");
        }
        state_ref.cwd.clone()
    };

    // Re-parse args (already validated above)
    let args: Vec<String> = if args_json.is_empty() {
        vec![]
    } else {
        serde_json::from_str(args_json).unwrap()
    };

    // Execute command in the project root directory.
    match std::process::Command::new(cmd)
        .args(&args)
        .current_dir(&cwd)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);
            json!({ "stdout": stdout, "stderr": stderr, "exitCode": exit_code })
        }
        Err(e) => err!("failed to execute '{cmd}': {e}"),
    }
}

// ── HTTP via AsyncDispatcher ──────────────────────────────────────────────

/// Execute an HTTP request on the shared async dispatcher.
///
/// Builds a reqwest client, executes the request, and returns the JSON response.
/// This replaces the dedicated `HttpExecutor` thread — the shared
/// [`AsyncDispatcher`](crate::async_dispatcher::AsyncDispatcher) handles
/// the async-to-sync bridging.
fn execute_http_request(request: reqwest::Request, max_bytes: u64) -> Result<String, String> {
    let dispatcher = crate::async_dispatcher::AsyncDispatcher::global();
    dispatcher.block_on(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())?;

        let response = client.execute(request).await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();

        let mut headers_map = serde_json::Map::new();
        for (key, value) in response.headers() {
            if let Ok(v) = value.to_str() {
                headers_map.insert(
                    key.as_str().to_string(),
                    serde_json::Value::String(v.to_string()),
                );
            }
        }

        let body_bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if body_bytes.len() > usize::try_from(max_bytes).unwrap_or(usize::MAX) {
            return Err(format!(
                "response body too large ({} bytes, max {max_bytes})",
                body_bytes.len()
            ));
        }

        let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

        serde_json::to_string(&serde_json::json!({
            "status": status,
            "headers": headers_map,
            "body": body_text,
        }))
        .map_err(|e| e.to_string())
    })
}

/// Make an HTTP request from an extension (if permitted).
///
/// Accepts a JSON options object with:
/// - `url` (required): The URL to fetch.
/// - `method` (optional): HTTP method, defaults to `"GET"`.
/// - `headers` (optional): Object of header name → value strings.
/// - `body` (optional): Request body string.
/// - `max_bytes` (optional): Maximum response body size in bytes (default 10 MiB).
///
/// Returns a JSON object `{ "status": N, "headers": {...}, "body": "..." }`,
/// or an error string starting with `__ERROR__`.
#[op2]
#[string]
fn op_rho_fetch_url(state: &mut OpState, #[string] opts_json: &str) -> String {
    require_perm!(state, allow_network, "network");
    let url_str = require_field!(opts_json, "url");

    let opts: serde_json::Value = serde_json::from_str(opts_json).unwrap_or_default();
    let method = opts["method"].as_str().unwrap_or("GET");
    let max_bytes = opts["max_bytes"].as_u64().unwrap_or(10 * 1024 * 1024);
    let body = opts["body"].as_str().unwrap_or("");

    let req = match build_fetch_request(&url_str, method, body, &opts) {
        Ok(r) => r,
        Err(e) => return err!("{e}"),
    };

    match execute_http_request(req, max_bytes) {
        Ok(json) => json,
        Err(e) => err!("{e}"),
    }
}

/// Parse a string field from a JSON object.
///
/// Deprecated: use the [`require_field!`] macro instead.
#[allow(dead_code)]
fn parse_field(json: &str, field: &str) -> Option<String> {
    let opts: serde_json::Value = serde_json::from_str(json).ok()?;
    opts[field].as_str().map(String::from)
}

/// Build a `reqwest::Request` from the given parameters.
///
/// Validates the URL scheme (http/https only) and the HTTP method.
/// Applies custom headers from the `opts` JSON object.
fn build_fetch_request(
    url_str: &str,
    method: &str,
    body: &str,
    opts: &serde_json::Value,
) -> Result<reqwest::Request, String> {
    let parsed_url = url::Url::parse(url_str).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(format!(
            "unsupported URL scheme '{}' (only http and https are allowed)",
            parsed_url.scheme()
        ));
    }

    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| format!("invalid HTTP method: {e}"))?;

    let mut req_builder = reqwest::Request::new(method, parsed_url);

    if !body.is_empty() {
        *req_builder.body_mut() = Some(reqwest::Body::from(body.to_string()));
    }

    if let Some(headers) = opts["headers"].as_object() {
        for (key, value) in headers {
            let Some(val_str) = value.as_str() else {
                continue;
            };
            let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) else {
                continue;
            };
            if let Ok(val) = reqwest::header::HeaderValue::from_str(val_str) {
                req_builder.headers_mut().insert(name, val);
            }
        }
    }

    Ok(req_builder)
}

// ── URL ops ──────────────────────────────────────────────────────────────────

/// `op_rho_url_parse(spec, base)` — parse a URL string.
///
/// If `base` is non-empty, `spec` is resolved relative to `base`.
/// Returns a JSON object with URL components, or an error string.
#[op2]
#[string]
fn op_rho_url_parse(#[string] spec: &str, #[string] base: &str) -> String {
    let url_result = if base.is_empty() {
        Url::parse(spec)
    } else {
        let base_url = match Url::parse(base) {
            Ok(u) => u,
            Err(e) => return err!("invalid base URL: {e}"),
        };
        base_url.join(spec)
    };

    let url = match url_result {
        Ok(u) => u,
        Err(e) => return err!("invalid URL: {e}"),
    };

    let password = url.password().unwrap_or("").to_string();
    let query = url.query().map_or_else(String::new, |q| format!("?{q}"));
    let fragment = url.fragment().map_or_else(String::new, |f| format!("#{f}"));
    let port_str = url.port().map_or_else(String::new, |p| p.to_string());
    let host_str = url.host_str().unwrap_or("");
    let host_with_port = if url.port().is_some() {
        format!("{host_str}:{}", url.port().unwrap())
    } else {
        host_str.to_string()
    };

    json!({
        "href": url.as_str(),
        "protocol": format!("{}:", url.scheme()),
        "username": url.username(),
        "password": password,
        "hostname": host_str,
        "port": port_str,
        "pathname": url.path(),
        "search": query,
        "hash": fragment,
        "origin": url.origin().ascii_serialization(),
        "host": host_with_port,
    })
}

/// `op_rho_url_parse_search_params(input)` — parse a URL query string.
///
/// Returns a JSON array of `["key", "value"]` pairs.
#[op2]
#[string]
fn op_rho_url_parse_search_params(#[string] input: &str) -> String {
    let pairs: Vec<[String; 2]> = url::form_urlencoded::parse(input.as_bytes())
        .map(|(k, v)| [k.into_owned(), v.into_owned()])
        .collect();
    json!(pairs)
}

/// `op_rho_url_serialize_search_params(pairs_json)` — serialize key/value pairs.
///
/// Takes a JSON array of `["key", "value"]` pairs and returns a URL-encoded
/// query string (without the leading `?`).
#[op2]
#[string]
fn op_rho_url_serialize_search_params(#[string] pairs_json: &str) -> String {
    let pairs: Vec<[String; 2]> = serde_json::from_str(pairs_json).unwrap_or_default();
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for pair in pairs {
        serializer.append_pair(&pair[0], &pair[1]);
    }
    serializer.finish()
}

// ── Extension definition ─────────────────────────────────────────────────────

// The `rho_host` deno_core extension.
//
// Registers the ops above and provides ESM shims that create the `rho`
// global object and standard Web APIs (`console`, `fetch`, `URL`, etc.)
// on `globalThis`.
//
// Usage: `rho_host::init()` → add to `RuntimeOptions.extensions`.
deno_core::extension!(
    rho_host,
    ops = [
        op_rho_log,
        op_rho_get_cwd,
        op_rho_get_project_root,
        op_rho_get_model,
        op_rho_read_file,
        op_rho_write_file,
        op_rho_run_command,
        op_rho_fetch_url,
        op_rho_url_parse,
        op_rho_url_parse_search_params,
        op_rho_url_serialize_search_params,
    ],
    js = [ dir "src", "std_shim.js", "host_shim.js" ],
);

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::JsRuntime;

    /// Helper: create a `JsRuntime` with the `rho_host` extension + `HostState`.
    /// In test context, cwd and `ext_root_dir` are the same directory.
    fn runtime_with_state(cwd: &str) -> JsRuntime {
        let rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        // Inject HostState (project root = ext dir = cwd)
        rt.op_state().borrow_mut().put(HostState::new(
            PathBuf::from(cwd),
            Path::new(cwd),
            vec![],
            CommandDenylist::default_powershell(),
        ));

        rt
    }

    /// Helper: create a `JsRuntime` with the `rho_host` extension but no `HostState`.
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

    /// Escape a path for safe embedding in a JavaScript single-quoted string literal.
    ///
    /// On Windows, `display()` produces backslash-separated paths which
    /// JavaScript interprets as escape sequences. Replacing `\` with `\\`
    /// prevents that.
    fn js_escape(path: impl std::fmt::Display) -> String {
        path.to_string().replace('\\', "\\\\")
    }

    #[test]
    fn rho_log_accepts_all_levels() {
        let mut rt = runtime_with_state("/tmp");
        for level in &["trace", "debug", "info", "warn", "error"] {
            eval(&mut rt, &format!(r#"rho.log("{level}", "msg")"#));
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
        let result = eval(&mut rt, r"rho.getCwd()");
        assert_eq!(result, "/my/custom/dir");
    }

    #[test]
    fn rho_get_cwd_falls_back_without_state() {
        let mut rt = runtime_without_state();
        let result = eval(&mut rt, r"rho.getCwd()");
        // Should return some valid absolute path string (current dir)
        let is_absolute = std::path::Path::new(&result).is_absolute();
        assert!(is_absolute, "expected absolute path, got: {result}");
    }

    #[test]
    fn rho_global_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof rho");
        assert_eq!(result, "object");
    }

    #[test]
    fn rho_global_has_expected_methods() {
        let mut rt = runtime_with_state("/tmp");
        for method in &[
            "log",
            "getCwd",
            "getModel",
            "readFile",
            "writeFile",
            "runCommand",
            "fetchUrl",
        ] {
            let result = eval(&mut rt, &format!(r"typeof rho.{method}"));
            assert_eq!(result, "function", "rho.{method} should be a function");
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
        let code = format!(r"rho.readFile('{}')", js_escape(file_path.display()));
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
        let code = format!(
            r"rho.readFile('{}')",
            js_escape(outside_dir.path().join("secret.txt").display())
        );
        let result = eval_or_error(&mut rt, &code);
        let err = result.unwrap_err();
        assert!(
            err.contains("outside"),
            "expected sandbox error, got: {err}"
        );
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
        assert!(
            err.contains("outside") || err.contains("cannot resolve"),
            "expected sandbox error, got: {err}"
        );
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
            dir1.path(),
            vec![dir2.path().to_path_buf()],
            CommandDenylist::default_powershell(),
        ));

        let code = format!(
            r"rho.readFile('{}')",
            js_escape(dir2.path().join("extra.txt").display())
        );
        let result = eval(&mut rt, &code);
        assert_eq!(result, "extra content");
    }

    // ── writeFile tests ─────────────────────────────────────────────────────

    #[test]
    fn write_file_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        // writeFile returns undefined (void), just check no error
        eval(
            &mut rt,
            r#"rho.writeFile("output.txt", "hello from write")"#,
        );

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
        eval(
            &mut rt,
            r#"rho.writeFile("deep/nested/dir/file.txt", "deep content")"#,
        );

        let written = std::fs::read_to_string(dir.path().join("deep/nested/dir/file.txt")).unwrap();
        assert_eq!(written, "deep content");
    }

    #[test]
    fn write_file_absolute_path_within_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("abs.txt");

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(
            r#"rho.writeFile('{}', "abs content")"#,
            js_escape(file_path.display())
        );
        eval(&mut rt, &code);

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "abs content");
    }

    #[test]
    fn write_file_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());
        let code = format!(
            r#"rho.writeFile('{}/evil.txt', "pwned")"#,
            outside_dir.path().display()
        );
        let result = eval_or_error(&mut rt, &code);
        let err = result.unwrap_err();
        assert!(
            err.contains("outside"),
            "expected sandbox error, got: {err}"
        );
    }

    // ── round-trip test ─────────────────────────────────────────────────────

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();

        let mut rt = runtime_with_state(dir.path().to_str().unwrap());

        // Write
        eval(
            &mut rt,
            r#"rho.writeFile("roundtrip.json", JSON.stringify({a:1,b:2}))"#,
        );

        // Read back
        let result = eval(&mut rt, r#"rho.readFile("roundtrip.json")"#);
        assert_eq!(result, r#"{"a":1,"b":2}"#);
    }

    // ── runCommand tests ────────────────────────────────────────────────────

    /// Helper: create a `JsRuntime` with command execution enabled.
    fn runtime_with_commands(cwd: &str) -> JsRuntime {
        let rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        rt.op_state().borrow_mut().put(
            HostState::new(
                PathBuf::from(cwd),
                Path::new(cwd),
                vec![],
                CommandDenylist::default_powershell(),
            )
            .with_commands(true),
        );

        rt
    }

    #[test]
    fn run_command_echo() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        let result = eval(
            &mut rt,
            r#"JSON.stringify(rho.runCommand("pwsh", ["-Command", "Write-Output 'hello world'"]))"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 0);
        assert!(parsed["stdout"].as_str().unwrap().contains("hello world"));
    }

    #[test]
    fn run_command_no_args() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        // Use a command that exists as a standalone executable on all platforms.
        let result = eval(&mut rt, r#"JSON.stringify(rho.runCommand("hostname"))"#);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["exitCode"], 0);
        // hostname should return a non-empty string
        assert!(!parsed["stdout"].as_str().unwrap().trim().is_empty());
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

        let result = eval_or_error(&mut rt, r#"rho.runCommand("no_such_binary_xyz_123")"#);
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

        let result = eval_or_error(&mut rt, r#"rho.runCommand("echo", ["hello"])"#);
        let err = result.unwrap_err();
        assert!(
            err.contains("command execution permission"),
            "expected permission error, got: {err}"
        );
    }

    #[test]
    fn run_command_blocked_without_host_state() {
        let mut rt = runtime_without_state();

        let result = eval_or_error(&mut rt, r#"rho.runCommand("echo", ["hello"])"#);
        let err = result.unwrap_err();
        assert!(
            err.contains("command execution permission"),
            "expected permission error, got: {err}"
        );
    }

    #[test]
    fn run_command_blocked_by_denylist() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        // curl is on the default PowerShell denylist
        let result = eval_or_error(
            &mut rt,
            r#"rho.runCommand("curl", ["https://example.com"])"#,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("command denied"),
            "expected denylist error, got: {err}"
        );
        assert!(
            err.contains("curl"),
            "error should mention the denied command: {err}"
        );
    }

    #[test]
    fn run_command_blocked_by_denylist_flag_combo() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = runtime_with_commands(dir.path().to_str().unwrap());

        // -Recurse + -Force is a denied flag combination
        let result = eval_or_error(
            &mut rt,
            r#"rho.runCommand("Get-ChildItem", ["-Recurse", "-Force", "-Path", "foo"])"#,
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("command denied"),
            "expected denylist error, got: {err}"
        );
        assert!(
            err.contains("flag combination"),
            "error should mention flag combination: {err}"
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
            HostState::new(
                PathBuf::from("/tmp"),
                Path::new("/tmp"),
                vec![],
                CommandDenylist::default_powershell(),
            )
            .with_model("claude-sonnet-4-20250514"),
        );

        let result = eval(&mut rt, r"rho.getModel()");
        assert_eq!(result, "claude-sonnet-4-20250514");
    }

    #[test]
    fn get_model_returns_empty_without_host_state() {
        let mut rt = runtime_without_state();
        let result = eval(&mut rt, r"rho.getModel()");
        assert_eq!(result, "");
    }

    #[test]
    fn get_model_returns_empty_by_default() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"rho.getModel()");
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
        let host_state = HostState::new(
            PathBuf::from("/tmp"),
            Path::new("/tmp"),
            vec![],
            CommandDenylist::default_powershell(),
        )
        .with_model("gpt-4o");
        // We need to replace the model Arc with our shared one
        {
            let op_state = rt.op_state();
            op_state.borrow_mut().put(HostState {
                cwd: PathBuf::from("/tmp"),
                project_root: PathBuf::from("/tmp"),
                allowed_paths: host_state.allowed_paths.clone(),
                allow_commands: false,
                allow_network: false,
                denylist: host_state.denylist.clone(),
                model: model_clone,
            });
        }

        // Initial value
        let result = eval(&mut rt, r"rho.getModel()");
        assert_eq!(result, "gpt-4o");

        // Update from outside
        *model.lock().unwrap() = "claude-sonnet-4-20250514".to_string();

        // Extension should see the new value
        let result = eval(&mut rt, r"rho.getModel()");
        assert_eq!(result, "claude-sonnet-4-20250514");
    }

    // =========================================================================
    // Phase 2: Standard Web API shims
    // =========================================================================

    // -- console tests ---------------------------------------------------------

    #[test]
    fn console_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof console");
        assert_eq!(result, "object");
    }

    #[test]
    fn console_has_standard_methods() {
        let mut rt = runtime_with_state("/tmp");
        for method in &[
            "log",
            "debug",
            "info",
            "warn",
            "error",
            "trace",
            "assert",
            "clear",
            "dir",
            "table",
            "count",
            "countReset",
            "time",
            "timeEnd",
            "group",
            "groupEnd",
            "groupCollapsed",
        ] {
            let result = eval(&mut rt, &format!(r"typeof console.{method}"));
            assert_eq!(result, "function", "console.{method} should be a function");
        }
    }

    #[test]
    fn console_log_does_not_crash() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"console.log('hello from console')");
    }

    #[test]
    fn console_log_serializes_objects() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"console.log({ key: 'value' })");
    }

    #[test]
    fn console_error_does_not_crash() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"console.error('error message')");
    }

    #[test]
    fn console_assert_passes() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"console.assert(true, 'should not fire')");
    }

    #[test]
    fn console_time_and_time_end_share_state() {
        let mut rt = runtime_with_state("/tmp");
        // Monkey-patch console.log to capture the last logged message
        let result = eval(
            &mut rt,
            r#"
            let lastLog = "";
            const origLog = console.log;
            console.log = (...args) => { lastLog = args.join(" "); };
            console.time("my-timer");
            console.timeEnd("my-timer");
            // If timers are shared, lastLog should be "my-timer: Nms" (N >= 0)
            // If they are NOT shared, lastLog will still be "" because timeEnd
            // won't find the timer in its own separate map and does nothing.
            const found = lastLog.startsWith("my-timer:");
            console.log = origLog; // restore
            found
            "#,
        );
        assert_eq!(
            result, "true",
            "console.timeEnd should find the timer set by console.time (shared state)"
        );
    }

    // -- URL tests --------------------------------------------------------------

    #[test]
    fn url_class_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof URL");
        assert_eq!(result, "function");
    }

    #[test]
    fn url_parses_absolute() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const u = new URL("https://example.com:8080/path?q=hello#section");
            JSON.stringify({
                href: u.href,
                protocol: u.protocol,
                hostname: u.hostname,
                port: u.port,
                pathname: u.pathname,
                search: u.search,
                hash: u.hash,
                origin: u.origin,
            })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["href"],
            "https://example.com:8080/path?q=hello#section"
        );
        assert_eq!(parsed["protocol"], "https:");
        assert_eq!(parsed["hostname"], "example.com");
        assert_eq!(parsed["port"], "8080");
        assert_eq!(parsed["pathname"], "/path");
        assert_eq!(parsed["search"], "?q=hello");
        assert_eq!(parsed["hash"], "#section");
        assert_eq!(parsed["origin"], "https://example.com:8080");
    }

    #[test]
    fn url_parses_relative_with_base() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const u = new URL("/other", "https://example.com/path/page");
            u.href
        "#,
        );
        assert_eq!(result, "https://example.com/other");
    }

    #[test]
    fn url_throws_on_invalid() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval_or_error(&mut rt, r"new URL('not a url')");
        let err = result.unwrap_err();
        assert!(
            err.contains("invalid URL"),
            "expected URL parse error, got: {err}"
        );
    }

    #[test]
    fn url_to_string_and_to_json() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const u = new URL("https://example.com/path");
            u.toString() === u.href && u.toJSON() === u.href
        "#,
        );
        assert_eq!(result, "true");
    }

    #[test]
    fn url_username_and_password() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const u = new URL("https://user:pass@example.com");
            JSON.stringify({ user: u.username, pass: u.password })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["user"], "user");
        assert_eq!(parsed["pass"], "pass");
    }

    // -- URLSearchParams tests --------------------------------------------------

    #[test]
    fn url_search_params_class_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof URLSearchParams");
        assert_eq!(result, "function");
    }

    #[test]
    fn url_search_params_from_string() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("a=1&b=2&c=3");
            JSON.stringify({ a: sp.get('a'), b: sp.get('b'), c: sp.get('c') })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["a"], "1");
        assert_eq!(parsed["b"], "2");
        assert_eq!(parsed["c"], "3");
    }

    #[test]
    fn url_search_params_set_and_delete() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("a=1");
            sp.set('a', 'updated');
            sp.append('b', '2');
            sp.delete('b');
            JSON.stringify({ a: sp.get('a'), b: sp.get('b'), size: sp.size })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["a"], "updated");
        assert!(
            parsed["b"].is_null(),
            "expected null after delete, got: {}",
            parsed["b"]
        );
        assert_eq!(parsed["size"], 1);
    }

    #[test]
    fn url_search_params_to_string() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("key=value&foo=bar");
            sp.toString()
        "#,
        );
        assert_eq!(result, "key=value&foo=bar");
    }

    #[test]
    fn url_search_params_has_and_has_all() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("a=1&a=2&b=3");
            JSON.stringify({
                has_a: sp.has('a'),
                has_b: sp.has('b'),
                has_c: sp.has('c'),
                all_a: sp.getAll('a')
            })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["has_a"], true);
        assert_eq!(parsed["has_b"], true);
        assert_eq!(parsed["has_c"], false);
        assert_eq!(parsed["all_a"], serde_json::json!(["1", "2"]));
    }

    #[test]
    fn url_search_params_iteration() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("x=1&y=2");
            const entries = [...sp.entries()];
            const keys = [...sp.keys()];
            JSON.stringify({ entries, keys, size: sp.size })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            parsed["entries"],
            serde_json::json!([["x", "1"], ["y", "2"]])
        );
        assert_eq!(parsed["keys"], serde_json::json!(["x", "y"]));
        assert_eq!(parsed["size"], 2);
    }

    #[test]
    fn url_search_params_from_object() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const sp = new URLSearchParams({ a: '1', b: '2' });
            sp.toString()
        ",
        );
        assert_eq!(result, "a=1&b=2");
    }

    #[test]
    fn url_search_params_sort() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const sp = new URLSearchParams("c=3&a=1&b=2");
            sp.sort();
            sp.toString()
        "#,
        );
        assert_eq!(result, "a=1&b=2&c=3");
    }

    // -- Headers class tests -----------------------------------------------------

    #[test]
    fn headers_class_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof Headers");
        assert_eq!(result, "function");
    }

    #[test]
    fn headers_set_get_has() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const h = new Headers({ 'Content-Type': 'text/html' });
            h.set('X-Custom', 'value');
            JSON.stringify({
                ct: h.get('content-type'),
                custom: h.get('x-custom'),
                has_ct: h.has('content-type'),
                has_missing: h.has('missing'),
            })
        ",
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ct"], "text/html");
        assert_eq!(parsed["custom"], "value");
        assert_eq!(parsed["has_ct"], true);
        assert_eq!(parsed["has_missing"], false);
    }

    #[test]
    fn headers_case_insensitive() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const h = new Headers();
            h.set('Content-Type', 'application/json');
            h.get('content-type') // case-insensitive lookup
        ",
        );
        assert_eq!(result, "application/json");
    }

    #[test]
    fn headers_append() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const h = new Headers();
            h.append('Set-Cookie', 'a=1');
            h.append('Set-Cookie', 'b=2');
            h.get('set-cookie')
        ",
        );
        assert_eq!(result, "a=1, b=2");
    }

    #[test]
    fn headers_delete() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const h = new Headers({ 'X-Test': 'yes' });
            h.delete('x-test');
            h.has('x-test')
        ",
        );
        assert_eq!(result, "false");
    }

    // -- Response class tests ----------------------------------------------------

    #[test]
    fn response_class_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof Response");
        assert_eq!(result, "function");
    }

    #[test]
    fn response_ok_and_status() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const r = new Response("hello", { status: 200 });
            JSON.stringify({ ok: r.ok, status: r.status, statusText: r.statusText, body: r.body })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["statusText"], "OK");
        assert_eq!(parsed["body"], "hello");
    }

    #[test]
    fn response_error_status() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const r = new Response("not found", { status: 404 });
            JSON.stringify({ ok: r.ok, status: r.status, statusText: r.statusText })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["status"], 404);
        assert_eq!(parsed["statusText"], "Not Found");
    }

    #[test]
    fn response_json_method() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const r = Response.json({ key: "value" });
            JSON.stringify({ body: r.body, status: r.status })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["body"], r#"{"key":"value"}"#);
        assert_eq!(parsed["status"], 200);
    }

    #[test]
    fn response_static_methods() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const err = Response.error();
            const redir = Response.redirect("https://example.com");
            JSON.stringify({ err_status: err.status, redir_status: redir.status })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["err_status"], 0);
        assert_eq!(parsed["redir_status"], 302);
    }

    // -- Request class tests ----------------------------------------------------

    #[test]
    fn request_class_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof Request");
        assert_eq!(result, "function");
    }

    #[test]
    fn request_constructs() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const r = new Request("https://example.com/api", { method: "POST" });
            JSON.stringify({ url: r.url, method: r.method })
        "#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["url"], "https://example.com/api");
        assert_eq!(parsed["method"], "POST");
    }

    // -- fetch function tests ---------------------------------------------------

    #[test]
    fn fetch_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof fetch");
        assert_eq!(result, "function");
    }

    // Note: fetch() calls op_rho_fetch_url which requires network permission.
    // The actual HTTP functionality is tested in runtime::tests::fetch_url_works_with_network_permission.
    // Here we test that fetch() is a function and rejects without permission.

    #[test]
    fn fetch_blocked_without_network_permission() {
        let dir = tempfile::tempdir().unwrap();
        let mut rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });
        // Default HostState: no network permission
        rt.op_state().borrow_mut().put(HostState::new(
            dir.path().to_path_buf(),
            dir.path(),
            vec![],
            CommandDenylist::default_powershell(),
        ));

        // Use rho.fetchUrl (synchronous, throws immediately) rather than
        // fetch (async, errors are unhandled promise rejections).
        let result = eval_or_error(&mut rt, r"rho.fetchUrl({ url: 'https://example.com' })");
        let err = result.unwrap_err();
        assert!(
            err.contains("network permission"),
            "expected network permission error, got: {err}"
        );
    }

    // -- btoa / atob tests -------------------------------------------------------

    #[test]
    fn btoa_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof btoa");
        assert_eq!(result, "function");
    }

    #[test]
    fn atob_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof atob");
        assert_eq!(result, "function");
    }

    #[test]
    fn btoa_encodes_basic() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"btoa('hello')");
        assert_eq!(result, "aGVsbG8=");
    }

    #[test]
    fn atob_decodes_basic() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"atob('aGVsbG8=')");
        assert_eq!(result, "hello");
    }

    #[test]
    fn btoa_atob_roundtrip() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"atob(btoa('test string 123!'))");
        assert_eq!(result, "test string 123!");
    }

    #[test]
    fn btoa_atob_utf8_roundtrip() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"atob(btoa('hello world'))");
        assert_eq!(result, "hello world");
    }

    // -- setTimeout / setInterval stub tests --------------------------------------

    #[test]
    fn set_timeout_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof setTimeout");
        assert_eq!(result, "function");
    }

    #[test]
    fn set_timeout_returns_negative_id() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"setTimeout(() => {}, 1000)");
        assert_eq!(result, "-1");
    }

    #[test]
    fn set_interval_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof setInterval");
        assert_eq!(result, "function");
    }

    #[test]
    fn clear_timeout_does_not_crash() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"clearTimeout(123)");
    }

    #[test]
    fn clear_interval_does_not_crash() {
        let mut rt = runtime_with_state("/tmp");
        eval(&mut rt, r"clearInterval(456)");
    }

    // -- structuredClone test ----------------------------------------------------

    #[test]
    fn structured_clone_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof structuredClone");
        assert_eq!(result, "function");
    }

    #[test]
    fn structured_clone_clones_object() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"
            const obj = { a: 1, b: { c: 2 } };
            const clone = structuredClone(obj);
            clone.b.c = 99;
            obj.b.c === 2 && clone.b.c === 99
        ",
        );
        assert_eq!(result, "true");
    }

    // -- TextEncoder / TextDecoder tests ------------------------------------------

    #[test]
    fn text_encoder_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof TextEncoder");
        assert_eq!(result, "function");
    }

    #[test]
    fn text_decoder_is_defined() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"typeof TextDecoder");
        assert_eq!(result, "function");
    }

    #[test]
    fn text_encoder_decode_roundtrip() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const enc = new TextEncoder();
            const dec = new TextDecoder();
            dec.decode(enc.encode("hello world"))
        "#,
        );
        assert_eq!(result, "hello world");
    }

    // -- URL op direct tests ----------------------------------------------------

    #[test]
    fn url_op_parses_absolute_url() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const json = Deno.core.ops.op_rho_url_parse("https://user:pass@example.com:8080/path?q=1#frag", "");
            const parsed = JSON.parse(json);
            parsed.href === "https://user:pass@example.com:8080/path?q=1#frag" &&
            parsed.username === "user" && parsed.password === "pass" &&
            parsed.hostname === "example.com" && parsed.origin === "https://example.com:8080"
        "#,
        );
        assert_eq!(result, "true");
    }

    #[test]
    fn url_op_parses_with_base() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const json = Deno.core.ops.op_rho_url_parse("bar", "https://example.com/foo/");
            JSON.parse(json).href
        "#,
        );
        assert_eq!(result, "https://example.com/foo/bar");
    }

    #[test]
    fn url_op_rejects_invalid() {
        let mut rt = runtime_with_state("/tmp");
        // Direct op call returns __ERROR__ string (the JS URL class wraps this in unwrapOpResult)
        let result = eval(&mut rt, r"Deno.core.ops.op_rho_url_parse(':::invalid', '')");
        assert!(
            result.starts_with("__ERROR__"),
            "expected __ERROR__ prefix, got: {result}"
        );
    }

    #[test]
    fn url_op_parse_search_params() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            const json = Deno.core.ops.op_rho_url_parse_search_params("a=hello&b=world");
            const parsed = JSON.parse(json);
            parsed.length === 2 && parsed[0][0] === "a" && parsed[0][1] === "hello"
        "#,
        );
        assert_eq!(result, "true");
    }

    #[test]
    fn url_op_serialize_search_params() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r#"
            Deno.core.ops.op_rho_url_serialize_search_params(JSON.stringify([["x", "1"], ["y", "2"]]))
        "#,
        );
        assert_eq!(result, "x=1&y=2");
    }

    #[test]
    fn url_op_serialize_empty_params() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(
            &mut rt,
            r"Deno.core.ops.op_rho_url_serialize_search_params('[]')",
        );
        assert_eq!(result, "");
    }

    #[test]
    fn url_op_parse_empty_search_params() {
        let mut rt = runtime_with_state("/tmp");
        let result = eval(&mut rt, r"Deno.core.ops.op_rho_url_parse_search_params('')");
        assert_eq!(result, "[]");
    }
}
