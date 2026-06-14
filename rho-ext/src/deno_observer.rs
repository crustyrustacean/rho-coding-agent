//! `DenoObserver` — bridges TypeScript extension hooks into rho's [`AgentObserver`] trait.
//!
//! Each extension that declares `hooks` in its manifest can receive agent loop
//! events. [`DenoObserver`] wraps an [`ExtensionRuntime`] and forwards the
//! relevant observer callbacks to the extension's hook functions.
//!
//! # Hook mapping
//!
//! | `AgentObserver` method       | Extension hook    | Argument format                       |
//! |----------------------------|-------------------|---------------------------------------|
//! | `on_tool_call`             | `onToolCall`      | `{"toolName":"...","arguments":"..."}` |
//! | `on_tool_result`           | `onToolResult`    | `{"toolName":"...","output":"...","isError":false}` |
//!
//! `on_state_change`, `on_text_delta`, `on_reasoning_delta`, `on_tool_denied`,
//! and `on_approval_requested` are **not** forwarded to extensions — they are
//! UI-level concerns that extensions don't need.
//!
//! # `OnLoad`
//!
//! The `onLoad` hook is called immediately after [`DenoObserver::new`] creates
//! the observer, not through the `AgentObserver` trait. Call
//! [`DenoObserver::fire_on_load`] once after construction.
//!
//! # Error handling
//!
//! Hook failures are logged but never propagate to the agent loop. A broken
//! hook should not crash the agent.

use std::sync::Arc;

use async_trait::async_trait;
use rho_core::agent::AgentObserver;
use rho_core::tool::ToolResult;
use tokio::sync::Mutex;
use tracing::warn;

use crate::ExtensionRuntime;

/// An [`AgentObserver`] that forwards agent loop events to a TypeScript extension.
///
/// Wraps an `Arc<Mutex<ExtensionRuntime>>` (same pattern as `DenoTool`).
/// Only hooks declared in the manifest are forwarded — the observer checks
/// which hooks exist before sending a request to the extension thread.
pub struct DenoObserver {
    /// The extension runtime (shared, thread-safe handle).
    runtime: Arc<Mutex<ExtensionRuntime>>,
}

impl DenoObserver {
    /// Create a new `DenoObserver` from a shared runtime handle.
    pub fn new(runtime: Arc<Mutex<ExtensionRuntime>>) -> Self {
        Self { runtime }
    }

    /// Fire the extension's `onLoad` hook, if declared.
    ///
    /// Call this once after creating the observer and registering tools.
    /// Errors are logged but not propagated.
    pub async fn fire_on_load(&self) {
        let rt = self.runtime.lock().await;
        if rt.manifest().hooks.on_load.is_none() {
            return;
        }
        if let Err(e) = rt.call_hook("onLoad", "").await {
            warn!(extension = %rt.manifest().name, error = %e, "onLoad hook failed");
        }
    }
}

#[async_trait]
impl AgentObserver for DenoObserver {
    async fn on_tool_call(&self, name: &str, arguments: &str) {
        let rt = self.runtime.lock().await;
        if rt.manifest().hooks.on_tool_call.is_none() {
            return;
        }

        let payload = serde_json::json!({
            "toolName": name,
            "arguments": arguments,
        })
        .to_string();

        if let Err(e) = rt.call_hook("onToolCall", &payload).await {
            warn!(
                extension = %rt.manifest().name,
                hook = "onToolCall",
                error = %e,
                "hook failed"
            );
        }
    }

    async fn on_tool_result(&self, name: &str, result: &ToolResult) {
        let rt = self.runtime.lock().await;
        if rt.manifest().hooks.on_tool_result.is_none() {
            return;
        }

        let payload = serde_json::json!({
            "toolName": name,
            "output": result.output,
            "isError": result.is_error,
        })
        .to_string();

        if let Err(e) = rt.call_hook("onToolResult", &payload).await {
            warn!(
                extension = %rt.manifest().name,
                hook = "onToolResult",
                error = %e,
                "hook failed"
            );
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtensionRuntime;

    /// Helper: spawn a runtime from inline TS.
    fn spawn_runtime(ts_source: &str) -> ExtensionRuntime {
        ExtensionRuntime::spawn(url::Url::parse("file:///test.ts").unwrap(), ts_source)
            .expect("spawn should succeed")
    }

    /// Helper: create an Arc<Mutex<ExtensionRuntime>> from inline TS.
    fn make_shared_runtime(ts_source: &str) -> Arc<Mutex<ExtensionRuntime>> {
        Arc::new(Mutex::new(spawn_runtime(ts_source)))
    }

    // =========================================================================
    // on_tool_call tests
    // =========================================================================

    #[tokio::test]
    async fn on_tool_call_forwards_to_hook() {
        let rt = make_shared_runtime(
            r#"
            let lastCall = "";
            export default {
                name: "hook-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => lastCall,
                }],
                hooks: {
                    onToolCall: async (args: string) => {
                        const { toolName } = JSON.parse(args);
                        lastCall = toolName;
                    },
                },
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());

        // Fire on_tool_call — now awaited directly
        observer
            .on_tool_call("search", r#"{"query":"hello"}"#)
            .await;

        // Verify the hook recorded the tool name by calling the extension's tool
        let result = rt.lock().await.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "search", "hook should have recorded 'search'");
    }

    #[tokio::test]
    async fn on_tool_call_skips_when_no_hook() {
        let rt = make_shared_runtime(
            r#"
            export default {
                name: "no-hook-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "untouched",
                }],
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());

        // Should not panic or do anything meaningful
        observer.on_tool_call("search", "{}").await;

        // Tool still works — hook didn't interfere
        let result = rt.lock().await.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "untouched");
    }

    // =========================================================================
    // on_tool_result tests
    // =========================================================================

    #[tokio::test]
    async fn on_tool_result_forwards_to_hook() {
        let rt = make_shared_runtime(
            r#"
            let lastResult = "";
            export default {
                name: "result-hook-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => lastResult,
                }],
                hooks: {
                    onToolResult: async (args: string) => {
                        const { toolName, output } = JSON.parse(args);
                        lastResult = `${toolName}:${output}`;
                    },
                },
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());

        observer
            .on_tool_result("search", &ToolResult::success("found 3 items"))
            .await;

        // Verify hook captured the result
        let result = rt.lock().await.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "search:found 3 items");
    }

    #[tokio::test]
    async fn on_tool_result_sends_is_error_flag() {
        let rt = make_shared_runtime(
            r#"
            let captured = "";
            export default {
                name: "error-flag-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => captured,
                }],
                hooks: {
                    onToolResult: async (args: string) => {
                        const { isError } = JSON.parse(args);
                        captured = String(isError);
                    },
                },
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());

        observer
            .on_tool_result("boom", &ToolResult::error("kaboom"))
            .await;

        let result = rt.lock().await.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "true");
    }

    // =========================================================================
    // onLoad tests
    // =========================================================================

    #[tokio::test]
    async fn fire_on_load_calls_hook() {
        let rt = make_shared_runtime(
            r#"
            let loaded = false;
            export default {
                name: "onload-test",
                tools: [{
                    name: "ping",
                    description: "Ping",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => String(loaded),
                }],
                hooks: {
                    onLoad: async () => { loaded = true; },
                },
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());
        observer.fire_on_load().await;

        let result = rt.lock().await.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "true", "onLoad should have set loaded=true");
    }

    #[tokio::test]
    async fn fire_on_load_skips_when_no_hook() {
        let rt = make_shared_runtime(
            r#"
            export default {
                name: "no-onload-test",
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());
        // Should not panic
        observer.fire_on_load().await;
    }

    // =========================================================================
    // Lifecycle: shutdown then observe
    // =========================================================================

    #[tokio::test]
    async fn observer_after_shutdown_logs_error() {
        let rt = make_shared_runtime(
            r#"
            export default {
                name: "shutdown-test",
                hooks: {
                    onToolCall: async () => "ok",
                },
            };
            "#,
        );

        let observer = DenoObserver::new(rt.clone());

        // Shutdown the runtime
        rt.lock().await.shutdown().unwrap();

        // This should not panic — the hook call will log a warning
        observer.on_tool_call("test", "{}").await;
    }
}
