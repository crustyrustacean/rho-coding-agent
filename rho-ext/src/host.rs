//! Host functions exposed to extension JS via `rho.*` global.
//!
//! This module defines `deno_core` ops (`#[op2]`) and bundles them into a
//! `deno_core::Extension` named `"rho_host"`. An `OpState` cell
//! (`HostState`) is injected at runtime creation time so that ops can
//! access the extension's working directory and other shared context.
//!
//! Currently implemented host functions:
//!
//! | JS call | Rust op | Behaviour |
//! |---|---|---|
//! | `rho.log(level, msg)` | `op_rho_log` | Emit a structured log via `tracing` |
//! | `rho.getCwd()` | `op_rho_get_cwd` | Return the extension's working directory |
//!
//! More host functions (`readFile`, `writeFile`, `runCommand`) will be added
//! in follow-up tasks.

use std::path::PathBuf;

use deno_core::{OpState, op2};

// ── OpState cell ──────────────────────────────────────────────────────────────

/// Shared state available to all rho host ops.
///
/// Inserted into `OpState` via `state.put()` when the extension is
/// initialised. Individual ops borrow it via `state.borrow::<HostState>()`.
#[derive(Debug)]
pub struct HostState {
    /// The working directory rho was started in (or the extension's root dir).
    pub cwd: PathBuf,
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

// ── Extension definition ─────────────────────────────────────────────────────

// The `rho_host` deno_core extension.
//
// Registers the ops above and provides an ESM shim that creates the `rho`
// global object on `globalThis`.
//
// Usage: `rho_host::init()` → add to `RuntimeOptions.extensions`.
deno_core::extension!(
    rho_host,
    ops = [op_rho_log, op_rho_get_cwd],
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
        let rt = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        });

        // Inject HostState
        rt.op_state().borrow_mut().put(HostState {
            cwd: PathBuf::from(cwd),
        });

        rt
    }

    /// Helper: create a JsRuntime with the rho_host extension but no HostState.
    fn runtime_without_state() -> JsRuntime {
        JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![super::rho_host::init()],
            ..Default::default()
        })
    }

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
        for method in &["log", "getCwd"] {
            let result = eval(&mut rt, &format!(r#"typeof rho.{}"#, method));
            assert_eq!(result, "function", "rho.{} should be a function", method);
        }
    }
}
