//! `DenoTool` — bridges a TypeScript extension's tool into rho's [`Tool`] trait.
//!
//! Each [`LoadedTool`] in an extension's manifest becomes a [`DenoTool`] that
//! can be registered in rho-core's [`ToolRegistry`]. When the agent loop calls
//! [`Tool::execute`], the `DenoTool` forwards the call through
//! [`ExtensionRuntime::call_tool`] to the V8 isolate on the extension thread.

use std::sync::Arc;

use async_trait::async_trait;
use rho_core::error::Result;
use rho_core::newtypes::ToolName;
use rho_core::tool::{CancellationToken, Tool, ToolOutcome, ToolResult, ToolRisk as CoreToolRisk};
use tokio::sync::Mutex;

use crate::manifest::{LoadedTool, ToolRisk};

/// A [`Tool`] implementation backed by a TypeScript extension function.
///
/// `DenoTool` holds:
/// - **Metadata** from the extension's manifest (name, description, parameters, risk)
/// - **A reference** to the [`ExtensionRuntime`](crate::ExtensionRuntime) that owns
///   the V8 isolate, wrapped in `Arc<Mutex<>>` so the tool can be `Send + Sync`
///
/// When [`Tool::execute`] is called by the agent loop, the `DenoTool` serializes
/// the arguments to JSON and forwards them to the extension's `execute` function
/// via the runtime's channel. The result string is returned as a
/// [`ToolOutcome::Immediate`].
pub struct DenoTool {
    /// Tool metadata from the extension manifest.
    metadata: LoadedTool,
    /// The extension runtime (shared, thread-safe handle).
    runtime: Arc<Mutex<crate::ExtensionRuntime>>,
}

impl DenoTool {
    /// Create a new `DenoTool` from manifest metadata and a shared runtime handle.
    pub fn new(metadata: LoadedTool, runtime: Arc<Mutex<crate::ExtensionRuntime>>) -> Self {
        Self { metadata, runtime }
    }
}

/// Convert a manifest [`ToolRisk`] to rho-core's [`CoreToolRisk`].
fn to_core_risk(risk: ToolRisk) -> CoreToolRisk {
    match risk {
        ToolRisk::Read => CoreToolRisk::Read,
        ToolRisk::Write => CoreToolRisk::Write,
        ToolRisk::Destructive => CoreToolRisk::Destructive,
    }
}

/// Build a JSON Schema object from the extension's parameter definitions.
fn build_parameters_schema(
    params: &std::collections::HashMap<String, crate::manifest::ParameterDef>,
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for (name, def) in params {
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String(def.param_type.clone()),
        );
        if let Some(ref desc) = def.description {
            prop.insert(
                "description".to_string(),
                serde_json::Value::String(desc.clone()),
            );
        }
        properties.insert(name.clone(), serde_json::Value::Object(prop));

        if def.required {
            required.push(serde_json::Value::String(name.clone()));
        }
    }

    let mut schema = serde_json::Map::new();
    schema.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    if !properties.is_empty() {
        schema.insert(
            "properties".to_string(),
            serde_json::Value::Object(properties),
        );
    }
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::Value::Array(required));
    }

    serde_json::Value::Object(schema)
}

#[async_trait]
impl Tool for DenoTool {
    fn name(&self) -> ToolName {
        ToolName::from(self.metadata.name.clone())
    }

    fn description(&self) -> &str {
        &self.metadata.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        build_parameters_schema(&self.metadata.parameters)
    }

    fn risk(&self) -> CoreToolRisk {
        to_core_risk(self.metadata.risk)
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let args_json = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());

        let rt = self.runtime.lock().await;
        match rt.call_tool(&self.metadata.name, &args_json).await {
            Ok(output) => Ok(ToolOutcome::Immediate(ToolResult::success(output))),
            Err(crate::ExtensionError::Execution(msg)) => {
                Ok(ToolOutcome::Immediate(ToolResult::error(msg)))
            }
            Err(crate::ExtensionError::ToolNotFound(name)) => Ok(ToolOutcome::Immediate(
                ToolResult::error(format!("tool not found in extension: {name}")),
            )),
            Err(crate::ExtensionError::RuntimeShutdown) => Ok(ToolOutcome::Immediate(
                ToolResult::error("extension runtime shut down"),
            )),
            Err(e) => Ok(ToolOutcome::Immediate(ToolResult::error(e.to_string()))),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ParameterDef;
    use std::collections::HashMap;

    /// Helper: create an `ExtensionRuntime` from inline TS and wrap it.
    fn make_runtime_and_tool(
        ts_source: &str,
        tool_name: &str,
    ) -> (Arc<Mutex<crate::ExtensionRuntime>>, DenoTool) {
        let rt =
            crate::ExtensionRuntime::spawn(url::Url::parse("file:///test.ts").unwrap(), ts_source)
                .expect("spawn should succeed");

        let loaded_tool = rt
            .manifest()
            .tools
            .iter()
            .find(|t| t.name == tool_name)
            .expect("tool should exist in manifest")
            .clone();

        let rt = Arc::new(Mutex::new(rt));
        let tool = DenoTool::new(loaded_tool, rt.clone());
        (rt, tool)
    }

    // =========================================================================
    // Trait method tests
    // =========================================================================

    #[test]
    fn name_returns_metadata_name() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "my_tool",
                    description: "A tool",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
            "my_tool",
        );
        assert_eq!(&*tool.name(), "my_tool");
    }

    #[test]
    fn description_returns_metadata_description() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "t",
                    description: "Does a thing",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
            "t",
        );
        assert_eq!(tool.description(), "Does a thing");
    }

    #[test]
    fn risk_maps_correctly() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "t",
                    description: "Destructive",
                    risk: "destructive" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
            "t",
        );
        assert_eq!(tool.risk(), CoreToolRisk::Destructive);
    }

    #[test]
    fn parameters_schema_empty() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "t",
                    description: "T",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
            "t",
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        // No properties or required for empty params
        assert!(
            schema
                .get("properties")
                .is_none_or(|p| p.as_object().unwrap().is_empty())
        );
    }

    #[test]
    fn parameters_schema_with_fields() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "t",
                    description: "T",
                    risk: "read" as const,
                    parameters: {
                        query: { type: "string", description: "Search query", required: true },
                        limit: { type: "number", required: false },
                    },
                    execute: async () => "ok",
                }],
            };
            "#,
            "t",
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");

        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props["query"]["type"], "string");
        assert_eq!(props["query"]["description"], "Search query");
        assert_eq!(props["limit"]["type"], "number");

        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "query");
    }

    // =========================================================================
    // Execute tests
    // =========================================================================

    #[tokio::test]
    async fn execute_returns_success() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "echo",
                    description: "Echo",
                    risk: "read" as const,
                    parameters: {},
                    execute: async (args: string) => {
                        const { msg } = JSON.parse(args);
                        return msg;
                    },
                }],
            };
            "#,
            "echo",
        );

        let result = tool
            .execute(
                serde_json::json!({"msg": "hello"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(tr) => {
                assert!(!tr.is_error);
                assert_eq!(tr.output, "hello");
            }
            ToolOutcome::Streamed(_) => panic!("expected Immediate, got Streamed"),
        }
    }

    #[tokio::test]
    async fn execute_surfaces_js_error() {
        let (_rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "boom",
                    description: "Boom",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => { throw new Error("kaboom"); },
                }],
            };
            "#,
            "boom",
        );

        let result = tool
            .execute(serde_json::json!({}), CancellationToken::new())
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(tr) => {
                assert!(tr.is_error);
                assert!(
                    tr.output.contains("kaboom"),
                    "expected 'kaboom', got: {}",
                    tr.output
                );
            }
            ToolOutcome::Streamed(_) => panic!("expected Immediate"),
        }
    }

    #[tokio::test]
    async fn execute_after_shutdown_returns_error() {
        let (rt, tool) = make_runtime_and_tool(
            r#"
            export default {
                name: "test",
                tools: [{
                    name: "t",
                    description: "T",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "ok",
                }],
            };
            "#,
            "t",
        );

        // Shut down the runtime
        rt.lock().await.shutdown().unwrap();

        let result = tool
            .execute(serde_json::json!({}), CancellationToken::new())
            .await
            .unwrap();

        match result {
            ToolOutcome::Immediate(tr) => {
                assert!(tr.is_error);
                assert!(
                    tr.output.contains("shut down"),
                    "expected shutdown error, got: {}",
                    tr.output
                );
            }
            ToolOutcome::Streamed(_) => panic!("expected Immediate"),
        }
    }

    // =========================================================================
    // build_parameters_schema unit tests
    // =========================================================================

    #[test]
    fn schema_empty_params() {
        let schema = build_parameters_schema(&HashMap::new());
        assert_eq!(schema["type"], "object");
        assert!(schema.get("properties").is_none());
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn schema_single_required_param() {
        let mut params = HashMap::new();
        params.insert(
            "query".to_string(),
            ParameterDef {
                param_type: "string".to_string(),
                description: Some("Search query".to_string()),
                required: true,
            },
        );

        let schema = build_parameters_schema(&params);
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["properties"]["query"]["description"], "Search query");
        assert_eq!(schema["required"][0], "query");
    }

    // =========================================================================
    // Risk mapping tests
    // =========================================================================

    #[test]
    fn risk_mapping() {
        assert_eq!(to_core_risk(ToolRisk::Read), CoreToolRisk::Read);
        assert_eq!(to_core_risk(ToolRisk::Write), CoreToolRisk::Write);
        assert_eq!(
            to_core_risk(ToolRisk::Destructive),
            CoreToolRisk::Destructive
        );
    }
}
