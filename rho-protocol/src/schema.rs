//! `OpenRPC` schema generation for the rho JSON-RPC 2.0 protocol.
//!
//! Behind the `schema` cargo feature so frontends that don't generate
//! clients pay nothing for it. `cargo xtask schema` consumes this module to
//! emit `docs/rpc-schema/openrpc.json` from the same wire structs the
//! dispatch layer uses — the schema can no longer drift from the code,
//! because it *is* the code.
//!
//! The registry below is the single place a new method must be registered:
//! add the entry, and both the generated schema and the dispatch-consistency
//! test pick it up.

#![allow(clippy::doc_markdown)]
#[allow(
    clippy::wildcard_imports,
    reason = "registry references every wire type"
)]
use crate::types::*;
use schemars::JsonSchema;
use serde_json::Value;

/// One generated method entry: name, prose description, params type, result type.
struct MethodSpec {
    /// JSON-RPC method name, e.g. `"prompt"`.
    name: &'static str,
    /// Human-readable summary carried into the schema.
    description: &'static str,
    /// Params schema producer; `None` for methods that take no params.
    params: Option<fn() -> Value>,
    /// Result schema producer.
    result: fn() -> Value,
}

/// One generated notification entry.
struct NotificationSpec {
    /// Notification name, e.g. `"agent/end"`.
    name: &'static str,
    /// Human-readable summary carried into the schema.
    description: &'static str,
    /// Params schema producer; `None` for notifications that carry no params.
    params: Option<fn() -> Value>,
}

/// Serialize the `schemars` schema for `T` as a JSON value.
fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("serialize schema")
}

/// Recursively collect every `#/$defs/...` name referenced anywhere inside
/// `value`, so the root document can carry the definitions they point to.
fn collect_defs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k == "$ref"
                    && let Some(target) = v.as_str()
                    && let Some(name) = target.strip_prefix("#/$defs/")
                {
                    if !out.contains(&name.to_owned()) {
                        out.push(name.to_owned());
                    }
                } else {
                    collect_defs(v, out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_defs(item, out);
            }
        }
        _ => {}
    }
}

/// The `$defs` map for the root document: every wire struct referenced via
/// `$ref` from any method or notification, keyed by type name.
fn root_defs() -> Value {
    // Build the doc *without* $defs first to avoid recursion.
    let doc = serde_json::json!({
        "methods": methods(),
        "notifications": notifications(),
    });
    let mut names: Vec<String> = Vec::new();
    collect_defs(&doc, &mut names);

    let mut defs = serde_json::Map::new();
    for name in names {
        let schema = match name.as_str() {
            "ApiUsageWire" => schema_of::<ApiUsageWire>(),
            "ExtensionEntry" => schema_of::<ExtensionEntry>(),
            "ModelEntry" => schema_of::<ModelEntry>(),
            "PhaseTokenDistWire" => schema_of::<PhaseTokenDistWire>(),
            "ProviderEntry" => schema_of::<ProviderEntry>(),
            "ResolutionTokenDist" => schema_of::<ResolutionTokenDist>(),
            "RoleTokenDist" => schema_of::<RoleTokenDist>(),
            "SessionEntry" => schema_of::<SessionEntry>(),
            "TokenUsageWire" => schema_of::<TokenUsageWire>(),
            "ToolCallRecordWire" => schema_of::<ToolCallRecordWire>(),
            "ToolEntry" => schema_of::<ToolEntry>(),
            "UsageContextWire" => schema_of::<UsageContextWire>(),
            "UsageDeltaWire" => schema_of::<UsageDeltaWire>(),
            _ => continue,
        };
        let mut schema = match schema {
            Value::Object(mut obj) => {
                obj.remove("$schema");
                Value::Object(obj)
            }
            other => other,
        };
        // Nested refs inside a def (e.g. ToolCallRecordWire ->
        // ToolCallOutcomeWire) are already inline in schemars' output when
        // the target type has no schema_of entry above; nothing to do.
        if let Value::Object(obj) = &mut schema {
            obj.remove("title");
        }
        defs.insert(name, schema);
    }
    Value::Object(defs)
}

/// Build the JSON Schema `params` array for a request-params struct.
fn params_spec<T: JsonSchema>() -> Value {
    let schema = schema_of::<T>();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    // Field descriptions come from doc comments via schemars; match each
    // property name to its description from the schema's own metadata.
    let mut out = Vec::new();
    for (name, prop) in props {
        let description = prop
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let required = required.contains(&name);
        // Strip the description out of the property schema itself; OpenRPC
        // carries it at the parameter level.
        let mut prop = prop.clone();
        if let Some(obj) = prop.as_object_mut() {
            obj.remove("description");
        }
        out.push(serde_json::json!({
            "name": name,
            "description": description,
            "required": required,
            "schema": prop,
        }));
    }
    Value::Array(out)
}

/// Wrap a result type as the OpenRPC `result` object.
fn result_spec<T: JsonSchema>() -> Value {
    let mut schema = schema_of::<T>();
    let description = schema
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("description");
    }
    serde_json::json!({
        "name": "result",
        "description": description,
        "schema": schema,
    })
}

// ── Method registry ───────────────────────────────────────────────────────────

/// Every dispatchable method, in registry order.
///
/// # Panics
///
/// Never; the closures only serialize schema objects.
#[allow(
    clippy::too_many_lines,
    reason = "registry table; one entry per method"
)]
#[must_use]
pub fn methods() -> Vec<Value> {
    let specs: Vec<MethodSpec> = vec![
        MethodSpec {
            name: "prompt",
            description: "Send a user message to the agent. The agent runs its loop (send to model → tool calls → approval → execution → repeat) until it produces a text reply or exhausts a budget.",
            params: Some(params_spec::<PromptParams>),
            result: result_spec::<PromptResult>,
        },
        MethodSpec {
            name: "abort",
            description: "Cancel the in-progress turn, if any. Returns an empty acknowledgement; observe `agent/end` (finishReason `cancelled`) or `agent/error` for the outcome.",
            params: None,
            result: result_spec::<EmptyResult>,
        },
        MethodSpec {
            name: "clear",
            description: "Clear conversation history: branch the session tree back to the root entry so prior turns remain on disk but leave the active path.",
            params: None,
            result: result_spec::<EmptyResult>,
        },
        MethodSpec {
            name: "newSession",
            description: "Start a fresh session, preserving model, provider, tools, token budget, and redactor settings.",
            params: None,
            result: result_spec::<NewSessionResult>,
        },
        MethodSpec {
            name: "compact",
            description: "Trigger context compaction now, regardless of the auto-compact threshold. Summarizes older entries to free context.",
            params: None,
            result: result_spec::<EmptyResult>,
        },
        MethodSpec {
            name: "getState",
            description: "Return the active model, provider, working directory, and message count.",
            params: None,
            result: result_spec::<GetStateResult>,
        },
        MethodSpec {
            name: "getMessages",
            description: "Return all conversation messages on the active session path.",
            params: None,
            result: result_spec::<GetMessagesResult>,
        },
        MethodSpec {
            name: "setModel",
            description: "Switch the active model by id or `provider:id`. On a bare id, each provider is probed to find who serves it.",
            params: Some(params_spec::<SetModelParams>),
            result: result_spec::<SetModelResult>,
        },
        MethodSpec {
            name: "listModels",
            description: "List available models from all configured providers.",
            params: None,
            result: result_spec::<ListModelsResult>,
        },
        MethodSpec {
            name: "listProviders",
            description: "List configured providers with local/remote and reachability status.",
            params: None,
            result: result_spec::<ListProvidersResult>,
        },
        MethodSpec {
            name: "getSessionStats",
            description: "Return context-window budget and token-distribution stats.",
            params: None,
            result: result_spec::<GetSessionStatsResult>,
        },
        MethodSpec {
            name: "listSessions",
            description: "List the most recent sessions for this project.",
            params: None,
            result: result_spec::<ListSessionsResult>,
        },
        MethodSpec {
            name: "listExtensions",
            description: "List loaded extensions and the tools each contributes.",
            params: None,
            result: result_spec::<ListExtensionsResult>,
        },
        MethodSpec {
            name: "reloadExtensions",
            description: "Hot-reload extensions from disk (mtime-based change detection).",
            params: None,
            result: result_spec::<EmptyResult>,
        },
        MethodSpec {
            name: "resumeSession",
            description: "Resume a previous session from its JSONL file.",
            params: Some(params_spec::<ResumeSessionParams>),
            result: result_spec::<ResumeSessionResult>,
        },
        MethodSpec {
            name: "listTools",
            description: "List registered tools with their schemas and risk levels.",
            params: None,
            result: result_spec::<ListToolsResult>,
        },
        MethodSpec {
            name: "approvalResponse",
            description: "Respond to an `approval/request`. `approved: false` with a `message` is a redirect: the user's instructions are injected and the tool batch is abandoned.",
            params: Some(params_spec::<ApprovalResponseParams>),
            result: result_spec::<EmptyResult>,
        },
    ];

    specs
        .into_iter()
        .map(|m| {
            let mut v = serde_json::json!({
                "name": m.name,
                "description": m.description,
                "result": (m.result)(),
            });
            if let Some(params) = m.params {
                v["params"] = params();
            } else {
                v["params"] = Value::Array(vec![]);
            }
            v
        })
        .collect()
}

/// Every notification rho emits, in registry order.
#[must_use]
pub fn notifications() -> Vec<Value> {
    let specs: Vec<NotificationSpec> = vec![
        NotificationSpec {
            name: "ready",
            description: "Emitted once on startup when the RPC loop is accepting requests.",
            params: Some(params_spec::<ReadyParams>),
        },
        NotificationSpec {
            name: "agent/start",
            description: "The agent began processing a prompt.",
            params: Some(params_spec::<AgentStartParams>),
        },
        NotificationSpec {
            name: "agent/end",
            description: "The agent finished a turn with a full structured result.",
            params: Some(params_spec::<AgentEndParams>),
        },
        NotificationSpec {
            name: "agent/error",
            description: "The agent loop encountered an error.",
            params: Some(params_spec::<AgentErrorParams>),
        },
        NotificationSpec {
            name: "state/change",
            description: "Loop state transition (thinking, executing_tool, awaiting_approval, idle).",
            params: Some(params_spec::<StateChangeParams>),
        },
        NotificationSpec {
            name: "message/delta",
            description: "Streaming assistant text chunk.",
            params: Some(params_spec::<MessageDeltaParams>),
        },
        NotificationSpec {
            name: "reasoning/delta",
            description: "Streaming reasoning (thinking) chunk.",
            params: Some(params_spec::<ReasoningDeltaParams>),
        },
        NotificationSpec {
            name: "tool/call",
            description: "The model requested a tool call.",
            params: Some(params_spec::<ToolCallParams>),
        },
        NotificationSpec {
            name: "tool/result",
            description: "A tool finished executing.",
            params: Some(params_spec::<ToolResultParams>),
        },
        NotificationSpec {
            name: "tool/denied",
            description: "A tool call was denied by the approval gate.",
            params: Some(params_spec::<ToolDeniedParams>),
        },
        NotificationSpec {
            name: "approval/request",
            description: "Approval required — send `approvalResponse` before the turn can proceed.",
            params: Some(params_spec::<ApprovalRequestParams>),
        },
        NotificationSpec {
            name: "usage",
            description: "Per-iteration token/cost delta and a live context snapshot.",
            params: Some(params_spec::<UsageParams>),
        },
    ];

    specs
        .into_iter()
        .map(|n| {
            let mut v = serde_json::json!({
                "name": n.name,
                "description": n.description,
            });
            v["params"] = match n.params {
                Some(p) => p(),
                None => Value::Array(vec![]),
            };
            v
        })
        .collect()
}

/// The complete OpenRPC document (without the version, which xtask injects
/// from the workspace `Cargo.toml`).
#[must_use]
pub fn openrpc() -> Value {
    serde_json::json!({
        "openrpc": "1.3.1",
        "info": {
            "title": "rho",
            "description": "Headless coding agent — JSON-RPC 2.0 protocol over stdin/stdout. Diagnostic output goes to stderr.",
        },
        "methods": methods(),
        "notifications": notifications(),
        "$defs": root_defs(),
    })
}

/// The method names in registry order — consumed by the dispatch-consistency
/// test and by xtask.
#[must_use]
pub fn method_names() -> Vec<&'static str> {
    vec![
        "prompt",
        "abort",
        "clear",
        "newSession",
        "compact",
        "getState",
        "getMessages",
        "setModel",
        "listModels",
        "listProviders",
        "getSessionStats",
        "listSessions",
        "listExtensions",
        "reloadExtensions",
        "resumeSession",
        "listTools",
        "approvalResponse",
    ]
}

/// The notification names in registry order.
#[must_use]
pub fn notification_names() -> Vec<&'static str> {
    vec![
        "ready",
        "agent/start",
        "agent/end",
        "agent/error",
        "state/change",
        "message/delta",
        "reasoning/delta",
        "tool/call",
        "tool/result",
        "tool/denied",
        "approval/request",
        "usage",
    ]
}
