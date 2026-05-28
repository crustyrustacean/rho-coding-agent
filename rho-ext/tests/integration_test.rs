//! End-to-end integration tests for the rho extension runtime.
//!
//! These tests exercise the full pipeline — discovery, spawn, DenoTool
//! registration in a ToolRegistry, execution through the Tool trait, and
//! observer hook dispatch — exactly what rho does at startup.
//!
//! Unlike the unit tests in `src/`, these tests use the public API only.

use std::sync::Arc;
use std::time::Duration;

use rho_core::tool::{CancellationToken, ToolOutcome, ToolRegistry, ToolResult};
use rho_core::newtypes::ToolName;
use rho_ext::deno_observer::DenoObserver;
use rho_ext::deno_tool::DenoTool;
use rho_ext::discover::{deduplicate, discover};
use rho_core::AgentObserver;
use rho_ext::ExtensionRuntime;
use tokio::sync::Mutex;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Write the greeter extension (single-file) into `dir`.
fn write_greeter(dir: &std::path::Path) {
    std::fs::write(
        dir.join("greeter.ts"),
        r#"
export default {
    name: "greeter",
    version: "1.0.0",
    tools: [{
        name: "greet",
        description: "Greet someone by name",
        risk: "read" as const,
        parameters: {
            name: { type: "string", description: "Who to greet", required: true },
        },
        execute: async (args: string) => {
            const { name } = JSON.parse(args);
            return `Hello, ${name}!`;
        },
    }],
    hooks: {
        onLoad: async () => {
            rho.log("info", "greeter extension loaded");
        },
        onToolCall: async (args: string) => {
            const { toolName } = JSON.parse(args);
            rho.log("debug", `tool called: ${toolName}`);
        },
    },
};
"#,
    )
    .unwrap();
}

/// Write the math_utils extension (multi-file with imports) into `dir`.
fn write_math_utils(dir: &std::path::Path) {
    let math_dir = dir.join("math_utils");
    std::fs::create_dir_all(&math_dir).unwrap();

    std::fs::write(
        math_dir.join("ops.ts"),
        r#"export function add(a: number, b: number): number { return a + b; }"#,
    )
    .unwrap();

    std::fs::write(
        math_dir.join("mod.ts"),
        r#"
import { add } from "./ops.ts";

export default {
    name: "math-utils",
    version: "2.0.0",
    tools: [{
        name: "compute",
        description: "Add two numbers",
        risk: "read" as const,
        parameters: {
            a: { type: "number", description: "First operand", required: true },
            b: { type: "number", description: "Second operand", required: true },
        },
        execute: async (args: string) => {
            const { a, b } = JSON.parse(args);
            return String(add(a, b));
        },
    }],
    commands: [{
        name: "version",
        description: "Show version",
        handler: async () => "2.0.0",
    }],
};
"#,
    )
    .unwrap();
}

// ── Test 1: Full Pipeline ─────────────────────────────────────────────────────

#[tokio::test]
async fn full_pipeline_discover_load_execute() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Write extension files to disk
    write_greeter(dir.path());
    write_math_utils(dir.path());

    // Also write a non-TS file and a hidden file — should be ignored
    std::fs::write(dir.path().join("README.md"), "# Extensions").unwrap();
    std::fs::write(dir.path().join(".hidden.ts"), "// hidden").unwrap();

    // 2. Discover extensions
    let discovered = discover(&[dir.path().to_path_buf()]).unwrap();
    let discovered = deduplicate(discovered);

    assert_eq!(
        discovered.len(),
        2,
        "should find greeter + math_utils, got: {:?}",
        discovered.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // 3. Spawn runtimes, register tools, create observers
    let mut registry = ToolRegistry::new();
    let mut observers: Vec<DenoObserver> = Vec::new();
    let mut runtimes: Vec<Arc<Mutex<ExtensionRuntime>>> = Vec::new();

    for ext in &discovered {
        let runtime = ExtensionRuntime::spawn_from_file(&ext.entry_path, &ext.root_dir)
            .expect("spawn should succeed");

        // Verify manifest metadata
        let manifest = runtime.manifest().clone();
        assert!(
            !manifest.name.is_empty(),
            "extension should have a non-empty name"
        );

        let runtime = Arc::new(Mutex::new(runtime));

        // Register each tool as a DenoTool
        for tool_meta in &manifest.tools {
            let deno_tool = DenoTool::new(tool_meta.clone(), runtime.clone());
            registry.register(Box::new(deno_tool));
        }

        // Create observer for hooks
        let observer = DenoObserver::new(runtime.clone());
        observers.push(observer);
        runtimes.push(runtime);
    }

    // Fire onLoad hooks
    for obs in &observers {
        obs.fire_on_load().await;
    }

    // 4. Execute through the Tool trait (like the agent loop does)

    // greeter.greet({"name": "world"})
    let greet_name = ToolName::from("greet");
    let greet_tool = registry
        .get_by_name(&greet_name)
        .expect("greet tool should be registered");

    let result = greet_tool
        .execute(serde_json::json!({"name": "world"}), CancellationToken::new())
        .await
        .unwrap();

    match result {
        ToolOutcome::Immediate(tr) => {
            assert!(!tr.is_error, "greet should succeed");
            assert_eq!(tr.output, "Hello, world!");
        }
        ToolOutcome::Streamed(_) => panic!("expected Immediate result"),
    }

    // math_utils.compute({"a": 21, "b": 21})
    let compute_name = ToolName::from("compute");
    let compute_tool = registry
        .get_by_name(&compute_name)
        .expect("compute tool should be registered");

    let result = compute_tool
        .execute(serde_json::json!({"a": 21, "b": 21}), CancellationToken::new())
        .await
        .unwrap();

    match result {
        ToolOutcome::Immediate(tr) => {
            assert!(!tr.is_error, "compute should succeed");
            assert_eq!(tr.output, "42");
        }
        ToolOutcome::Streamed(_) => panic!("expected Immediate result"),
    }

    // 5. Verify observer hooks fire without panic
    observers[0].on_tool_call("greet", r#"{"name":"world"}"#);
    tokio::time::sleep(Duration::from_millis(50)).await;

    observers[0].on_tool_result("greet", &ToolResult::success("Hello, world!"));
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 6. Verify tool definitions are included in tool_definitions()
    let defs = registry.tool_definitions();
    assert_eq!(defs.len(), 2, "should have 2 tool definitions");

    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"greet"),
        "tool definitions should include greet"
    );
    assert!(
        names.contains(&"compute"),
        "tool definitions should include compute"
    );

    // 7. Clean shutdown
    for rt in &runtimes {
        rt.lock().await.shutdown().unwrap();
    }
}

// ── Test 2: Extension Hook Blocks a Tool Call ─────────────────────────────────

#[tokio::test]
async fn extension_hook_can_block_tool_calls() {
    let dir = tempfile::tempdir().unwrap();

    // A gatekeeper extension that blocks force-flagged commands
    std::fs::write(
        dir.path().join("no_force.ts"),
        r#"
export default {
    name: "no-force",
    hooks: {
        onToolCall: async (args: string) => {
            const { toolName, arguments: cmdArgs } = JSON.parse(args);
            if (toolName === "run_command") {
                const parsed = JSON.parse(cmdArgs);
                if (parsed.command && parsed.command.includes("--force")) {
                    return JSON.stringify({
                        block: true,
                        reason: "force flags blocked by no-force extension",
                    });
                }
            }
            return undefined;
        },
    },
};
"#,
    )
    .unwrap();

    let runtime = ExtensionRuntime::spawn_from_file(
        &dir.path().join("no_force.ts"),
        dir.path(),
    )
    .expect("spawn should succeed");

    assert!(runtime.manifest().hooks.on_tool_call.is_some());

    let runtime = Arc::new(Mutex::new(runtime));

    // Simulate what DenoObserver does: build the payload and call the hook
    let blocked_payload = serde_json::json!({
        "toolName": "run_command",
        "arguments": r#"{"command":"git push --force"}"#
    })
    .to_string();

    let mut rt = runtime.lock().await;
    let result = rt.call_hook("onToolCall", &blocked_payload).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result)
        .expect("hook should return valid JSON");

    assert_eq!(parsed["block"], true, "should block --force commands");
    assert_eq!(parsed["reason"], "force flags blocked by no-force extension");

    // Verify it allows normal commands
    let allowed_payload = serde_json::json!({
        "toolName": "run_command",
        "arguments": r#"{"command":"git push"}"#
    })
    .to_string();

    let result = rt.call_hook("onToolCall", &allowed_payload).await.unwrap();
    // Returns undefined → empty string or "undefined"
    assert!(
        result.is_empty() || result == "undefined",
        "should allow normal commands, got: {result}"
    );

    rt.shutdown().unwrap();
}

// ── Test 3: Project-local Overrides User-level ────────────────────────────────

#[tokio::test]
async fn project_local_extension_overrides_user_level() {
    let user_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();

    // User-level extension: returns "user"
    std::fs::write(
        user_dir.path().join("search.ts"),
        r#"
export default {
    name: "search",
    version: "1.0.0",
    tools: [{
        name: "search",
        description: "Search (user version)",
        risk: "read" as const,
        parameters: {},
        execute: async () => "user-version",
    }],
};
"#,
    )
    .unwrap();

    // Project-local extension: returns "project"
    let proj_ext_dir = project_dir.path().join(".rho").join("extensions");
    std::fs::create_dir_all(&proj_ext_dir).unwrap();
    std::fs::write(
        proj_ext_dir.join("search.ts"),
        r#"
export default {
    name: "search",
    version: "2.0.0",
    tools: [{
        name: "search",
        description: "Search (project version)",
        risk: "read" as const,
        parameters: {},
        execute: async () => "project-version",
    }],
};
"#,
    )
    .unwrap();

    // Discover from both directories
    let discovered = discover(&[
        user_dir.path().to_path_buf(),
        proj_ext_dir.clone(),
    ])
    .unwrap();

    let discovered = deduplicate(discovered);
    assert_eq!(discovered.len(), 1, "should have one extension after dedup");

    // Project-local should win
    let ext = &discovered[0];
    assert_eq!(ext.name, "search");

    let mut runtime = ExtensionRuntime::spawn_from_file(&ext.entry_path, &ext.root_dir)
        .expect("spawn should succeed");

    let result = runtime.call_tool("search", "").await.unwrap();
    assert_eq!(
        result, "project-version",
        "project-local extension should override user-level"
    );

    runtime.shutdown().unwrap();
}

// ── Test 4: Two Extensions Coexist with Shared Registry ───────────────────────

#[tokio::test]
async fn two_extensions_coexist_in_shared_registry() {
    let dir = tempfile::tempdir().unwrap();

    write_greeter(dir.path());
    write_math_utils(dir.path());

    let discovered = discover(&[dir.path().to_path_buf()]).unwrap();
    let discovered = deduplicate(discovered);

    let mut registry = ToolRegistry::new();
    let mut runtimes: Vec<Arc<Mutex<ExtensionRuntime>>> = Vec::new();

    for ext in &discovered {
        let runtime = ExtensionRuntime::spawn_from_file(&ext.entry_path, &ext.root_dir)
            .expect("spawn should succeed");

        let runtime = Arc::new(Mutex::new(runtime));

        for tool_meta in &runtime.lock().await.manifest().tools.clone() {
            let deno_tool = DenoTool::new(tool_meta.clone(), runtime.clone());
            registry.register(Box::new(deno_tool));
        }

        runtimes.push(runtime);
    }

    // Interleave calls to both extensions
    let greet = registry.get_by_name(&ToolName::from("greet")).unwrap();
    let compute = registry.get_by_name(&ToolName::from("compute")).unwrap();

    let r1 = greet
        .execute(serde_json::json!({"name": "Alice"}), CancellationToken::new())
        .await
        .unwrap();
    let r2 = compute
        .execute(serde_json::json!({"a": 10, "b": 20}), CancellationToken::new())
        .await
        .unwrap();
    let r3 = greet
        .execute(serde_json::json!({"name": "Bob"}), CancellationToken::new())
        .await
        .unwrap();
    let r4 = compute
        .execute(serde_json::json!({"a": 100, "b": 200}), CancellationToken::new())
        .await
        .unwrap();

    assert_tool_output(&r1, "Hello, Alice!");
    assert_tool_output(&r2, "30");
    assert_tool_output(&r3, "Hello, Bob!");
    assert_tool_output(&r4, "300");

    for rt in &runtimes {
        rt.lock().await.shutdown().unwrap();
    }
}

/// Helper: assert a ToolOutcome::Immediate with the expected output.
fn assert_tool_output(outcome: &ToolOutcome, expected: &str) {
    match outcome {
        ToolOutcome::Immediate(tr) => {
            assert!(!tr.is_error, "tool should succeed");
            assert_eq!(tr.output, expected);
        }
        ToolOutcome::Streamed(_) => panic!("expected Immediate result"),
    }
}
