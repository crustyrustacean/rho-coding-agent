//! Extension manifest types and extraction.
//!
//! When a TypeScript extension is loaded as an ES module, its
//! `export default { ... }` is parsed into a [`LoadedExtension`]. This
//! struct describes the tools, hooks, and commands the extension provides.
//!
//! # Extension file format
//!
//! ```typescript
//! export default {
//!   name: "my-extension",
//!   version: "1.0.0",
//!   tools: [{
//!     name: "my_tool",
//!     description: "Does a thing",
//!     risk: "read" as const,
//!     parameters: {
//!       query: { type: "string", description: "Search query", required: true },
//!     },
//!     execute: async (args) => { return { output: "result" }; },
//!   }],
//!   hooks: {
//!     onLoad: async () => {},
//!     onToolCall: async (toolName, args) => {},
//!   },
//!   commands: [{
//!     name: "my-cmd",
//!     description: "A slash command",
//!     handler: (args) => {},
//!   }],
//! };
//! ```

use std::collections::HashMap;

use deno_core::v8::{HandleScope, Local, Object, PinnedRef};

// ── Rust-side types ───────────────────────────────────────────────────────────

/// A fully extracted extension ready for registration.
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    /// Extension name (from `name` field).
    pub name: String,
    /// Optional version string.
    pub version: Option<String>,
    /// Tools declared by the extension.
    pub tools: Vec<LoadedTool>,
    /// Hooks declared by the extension.
    pub hooks: LoadedHooks,
    /// Slash commands declared by the extension.
    pub commands: Vec<LoadedCommand>,
}

/// A tool extracted from the extension manifest.
///
/// The `execute` function lives in V8 — this struct holds only the
/// metadata needed to register the tool and build its JSON schema.
#[derive(Debug, Clone)]
pub struct LoadedTool {
    /// Tool name (must be unique across all extensions).
    pub name: String,
    /// Human-readable description for the model.
    pub description: String,
    /// Risk level.
    pub risk: ToolRisk,
    /// Parameter definitions keyed by name.
    pub parameters: HashMap<String, ParameterDef>,
}

/// Risk level for a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRisk {
    /// The tool only reads data.
    Read,
    /// The tool may create or modify files.
    Write,
    /// The tool may execute commands or cause irreversible effects.
    Destructive,
}

/// A single parameter definition.
#[derive(Debug, Clone)]
pub struct ParameterDef {
    /// JSON schema type: `"string"`, `"number"`, or `"boolean"`.
    pub param_type: String,
    /// Optional human description.
    pub description: Option<String>,
    /// Whether the parameter is required.
    pub required: bool,
}

/// Hooks extracted from the extension manifest.
///
/// Each field is `Some(hook_name)` if the extension declared that hook.
#[derive(Debug, Clone, Default)]
pub struct LoadedHooks {
    /// `hooks.onLoad` — called when the extension loads.
    pub on_load: Option<String>,
    /// `hooks.onToolCall` — called before a tool executes.
    pub on_tool_call: Option<String>,
    /// `hooks.onToolResult` — called after a tool produces a result.
    pub on_tool_result: Option<String>,
    /// `hooks.onBeforeModel` — called before sending messages to the model.
    pub on_before_model: Option<String>,
}

/// A slash command extracted from the extension manifest.
#[derive(Debug, Clone)]
pub struct LoadedCommand {
    /// Command name (without the leading `/`).
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
}

// ── Extraction error ─────────────────────────────────────────────────────────

/// Errors that can occur during manifest extraction.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The module namespace does not have a default export.
    #[error("extension module has no default export")]
    NoDefaultExport,

    /// The default export is not an object.
    #[error("extension default export is not an object")]
    DefaultExportNotObject,

    /// A required field is missing from the manifest.
    #[error("missing required field '{0}' in extension manifest")]
    MissingField(String),

    /// A field has an invalid type or value.
    #[error("invalid field '{field}': {reason}")]
    InvalidField { field: String, reason: String },

    /// A tool is missing a required field.
    #[error("tool at index {index} is missing field '{field}'")]
    ToolMissingField { index: usize, field: String },

    /// A tool has an invalid field value.
    #[error("tool at index {index} has invalid field '{field}': {reason}")]
    ToolInvalidField {
        index: usize,
        field: String,
        reason: String,
    },

    /// A V8 error occurred during extraction.
    #[error("V8 error during manifest extraction: {0}")]
    V8(String),
}

// ── Public extraction API ────────────────────────────────────────────────────

/// Extract a [`LoadedExtension`] from a V8 module namespace object.
///
/// Call this inside a `deno_core::scope!` block:
///
/// ```ignore
/// let namespace = rt.get_module_namespace(mod_id).unwrap();
/// deno_core::scope!(scope, rt);
/// let ns = v8::Local::<v8::Object>::new(scope, namespace);
/// let manifest = extract_manifest(scope, ns)?;
/// ```
///
/// # Errors
///
/// Returns [`ManifestError`] if the manifest is missing required fields
/// or has invalid values.
pub fn extract_manifest(
    scope: &mut PinnedRef<'_, HandleScope<'_>>,
    namespace: Local<Object>,
) -> Result<LoadedExtension, ManifestError> {
    internal::extract_manifest_inner(scope, namespace).map_err(Into::into)
}

// ── Internal extraction logic ────────────────────────────────────────────────

/// Internal extraction helpers.
mod internal {
    use super::{
        HandleScope, HashMap, LoadedCommand, LoadedExtension, LoadedHooks, LoadedTool, Local,
        ManifestError, Object, ParameterDef, PinnedRef, ToolRisk,
    };

    /// Internal error type — lighter than [`super::ManifestError`].
    #[derive(Debug)]
    pub(super) enum ExtractError {
        /// No default export found.
        NoDefaultExport,
        /// Default export is not an object.
        NotObject,
        /// Missing required field.
        MissingField(String),
        /// Invalid field value.
        InvalidField {
            /// Field name.
            field: String,
            /// Why it's invalid.
            reason: String,
        },
        /// Tool missing required field.
        ToolMissingField {
            /// Tool index in the tools array.
            index: usize,
            /// Missing field name.
            field: String,
        },
        /// Tool field has invalid value.
        ToolInvalidField {
            /// Tool index in the tools array.
            index: usize,
            /// Invalid field name.
            field: String,
            /// Why it's invalid.
            reason: String,
        },
        /// V8 error.
        V8(String),
    }

    impl From<ExtractError> for ManifestError {
        fn from(e: ExtractError) -> Self {
            match e {
                ExtractError::NoDefaultExport => ManifestError::NoDefaultExport,
                ExtractError::NotObject => ManifestError::DefaultExportNotObject,
                ExtractError::MissingField(f) => ManifestError::MissingField(f),
                ExtractError::InvalidField { field, reason } => {
                    ManifestError::InvalidField { field, reason }
                }
                ExtractError::ToolMissingField { index, field } => {
                    ManifestError::ToolMissingField { index, field }
                }
                ExtractError::ToolInvalidField {
                    index,
                    field,
                    reason,
                } => ManifestError::ToolInvalidField {
                    index,
                    field,
                    reason,
                },
                ExtractError::V8(msg) => ManifestError::V8(msg),
            }
        }
    }

    /// Core extraction logic.
    pub(super) fn extract_manifest_inner(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        namespace: Local<Object>,
    ) -> Result<LoadedExtension, ExtractError> {
        let default_key = v8_str(scope, "default")?;
        let default_val = namespace
            .get(scope, default_key.into())
            .ok_or(ExtractError::NoDefaultExport)?;

        if default_val.is_undefined() || default_val.is_null() {
            return Err(ExtractError::NoDefaultExport);
        }

        let manifest_obj =
            Local::<Object>::try_from(default_val).map_err(|_| ExtractError::NotObject)?;

        let name = required_string(scope, manifest_obj, "name")?;
        let version = optional_string(scope, manifest_obj, "version");
        let tools = extract_tools(scope, manifest_obj)?;
        let hooks = extract_hooks(scope, manifest_obj)?;
        let commands = extract_commands(scope, manifest_obj)?;

        Ok(LoadedExtension {
            name,
            version,
            tools,
            hooks,
            commands,
        })
    }

    // -- String helpers --

    /// Create a v8 string or fail.
    fn v8_str<'s>(
        scope: &mut PinnedRef<'s, HandleScope<'_>>,
        s: &str,
    ) -> Result<deno_core::v8::Local<'s, deno_core::v8::String>, ExtractError> {
        deno_core::v8::String::new(scope, s)
            .ok_or_else(|| ExtractError::V8(format!("failed to create v8 string '{s}'")))
    }

    /// Read a required string field from a v8 object.
    fn required_string(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        obj: Local<Object>,
        field: &str,
    ) -> Result<String, ExtractError> {
        let key = v8_str(scope, field)?;
        let val = obj
            .get(scope, key.into())
            .filter(|v| !v.is_undefined() && !v.is_null())
            .ok_or_else(|| ExtractError::MissingField(field.to_string()))?;

        Ok(val.to_rust_string_lossy(scope))
    }

    /// Read an optional string field from a v8 object.
    fn optional_string(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        obj: Local<Object>,
        field: &str,
    ) -> Option<String> {
        let key = deno_core::v8::String::new(scope, field)?;
        let val = obj.get(scope, key.into())?;
        if val.is_undefined() || val.is_null() {
            return None;
        }
        Some(val.to_rust_string_lossy(scope))
    }

    /// Extract the `tools` array from the manifest.
    fn extract_tools(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        manifest: Local<Object>,
    ) -> Result<Vec<LoadedTool>, ExtractError> {
        let key = v8_str(scope, "tools")?;
        let val = manifest.get(scope, key.into());

        if val.is_none_or(|v| v.is_undefined() || v.is_null()) {
            return Ok(Vec::new());
        }

        let tools_arr = Local::<deno_core::v8::Array>::try_from(val.unwrap()).map_err(|_| {
            ExtractError::InvalidField {
                field: "tools".to_string(),
                reason: "expected an array".into(),
            }
        })?;

        let mut tools = Vec::new();
        for i in 0..tools_arr.length() {
            let tool_obj = get_array_obj(scope, tools_arr, i, "tools")?;
            let name = required_tool_string(scope, tool_obj, "name", i as usize)?;
            let description = required_tool_string(scope, tool_obj, "description", i as usize)?;
            let risk_str = required_tool_string(scope, tool_obj, "risk", i as usize)?;

            let risk = match risk_str.as_str() {
                "read" => ToolRisk::Read,
                "write" => ToolRisk::Write,
                "destructive" => ToolRisk::Destructive,
                other => {
                    return Err(ExtractError::ToolInvalidField {
                        index: i as usize,
                        field: "risk".to_string(),
                        reason: format!(
                            "expected 'read', 'write', or 'destructive', got '{other}'"
                        ),
                    });
                }
            };

            let parameters = extract_parameters(scope, tool_obj, i as usize)?;

            tools.push(LoadedTool {
                name,
                description,
                risk,
                parameters,
            });
        }

        Ok(tools)
    }

    /// Read a required string field from a tool object (with index for errors).
    fn required_tool_string(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        obj: Local<Object>,
        field: &str,
        index: usize,
    ) -> Result<String, ExtractError> {
        let key = v8_str(scope, field)?;
        let val = obj
            .get(scope, key.into())
            .filter(|v| !v.is_undefined() && !v.is_null())
            .ok_or_else(|| ExtractError::ToolMissingField {
                index,
                field: field.to_string(),
            })?;
        Ok(val.to_rust_string_lossy(scope))
    }

    /// Extract parameter definitions from a tool's `parameters` object.
    fn extract_parameters(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        tool_obj: Local<Object>,
        tool_index: usize,
    ) -> Result<HashMap<String, ParameterDef>, ExtractError> {
        let key = v8_str(scope, "parameters")?;
        let val = tool_obj.get(scope, key.into());

        if val.is_none_or(|v| v.is_undefined() || v.is_null()) {
            return Ok(HashMap::new());
        }

        let parameters_obj = Local::<Object>::try_from(val.unwrap()).map_err(|_| {
            ExtractError::ToolInvalidField {
                index: tool_index,
                field: "parameters".to_string(),
                reason: "expected an object".into(),
            }
        })?;

        let Some(prop_names) = parameters_obj
            .get_own_property_names(scope, deno_core::v8::GetPropertyNamesArgs::default())
        else {
            return Ok(HashMap::new());
        };

        let mut params = HashMap::new();
        for i in 0..prop_names.length() {
            let prop_name_val = prop_names
                .get_index(scope, i)
                .unwrap_or_else(|| deno_core::v8::undefined(scope).into());
            let prop_name_str = prop_name_val.to_rust_string_lossy(scope);

            let param_val = parameters_obj
                .get(scope, prop_name_val)
                .unwrap_or_else(|| deno_core::v8::undefined(scope).into());

            let Ok(param_def) = Local::<Object>::try_from(param_val) else {
                continue;
            };

            let ptype = {
                let k = v8_str(scope, "type")?;
                let v = param_def
                    .get(scope, k.into())
                    .filter(|v| !v.is_undefined() && !v.is_null())
                    .ok_or_else(|| ExtractError::ToolInvalidField {
                        index: tool_index,
                        field: format!("parameters.{prop_name_str}.type"),
                        reason: "missing required field".into(),
                    })?;
                v.to_rust_string_lossy(scope)
            };

            let desc = optional_string(scope, param_def, "description");

            let required = {
                match deno_core::v8::String::new(scope, "required") {
                    Some(k) => match param_def.get(scope, k.into()) {
                        Some(v) => v.is_true(),
                        None => false,
                    },
                    None => false,
                }
            };

            params.insert(
                prop_name_str,
                ParameterDef {
                    param_type: ptype,
                    description: desc,
                    required,
                },
            );
        }

        Ok(params)
    }

    /// Extract hooks from the manifest.
    fn extract_hooks(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        manifest: Local<Object>,
    ) -> Result<LoadedHooks, ExtractError> {
        let key = v8_str(scope, "hooks")?;
        let val = manifest.get(scope, key.into());

        if val.is_none_or(|v| v.is_undefined() || v.is_null()) {
            return Ok(LoadedHooks::default());
        }

        let hooks_obj =
            Local::<Object>::try_from(val.unwrap()).map_err(|_| ExtractError::InvalidField {
                field: "hooks".to_string(),
                reason: "expected an object".into(),
            })?;

        Ok(LoadedHooks {
            on_load: optional_hook_fn(scope, hooks_obj, "onLoad"),
            on_tool_call: optional_hook_fn(scope, hooks_obj, "onToolCall"),
            on_tool_result: optional_hook_fn(scope, hooks_obj, "onToolResult"),
            on_before_model: optional_hook_fn(scope, hooks_obj, "onBeforeModel"),
        })
    }

    /// Check if a hook function exists and return its name.
    fn optional_hook_fn(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        hooks_obj: Local<Object>,
        hook_name: &str,
    ) -> Option<String> {
        let key = deno_core::v8::String::new(scope, hook_name)?;
        let val = hooks_obj.get(scope, key.into())?;
        if val.is_undefined() || val.is_null() {
            return None;
        }
        Some(hook_name.to_string())
    }

    /// Extract commands from the manifest.
    fn extract_commands(
        scope: &mut PinnedRef<'_, HandleScope<'_>>,
        manifest: Local<Object>,
    ) -> Result<Vec<LoadedCommand>, ExtractError> {
        let key = v8_str(scope, "commands")?;
        let val = manifest.get(scope, key.into());

        if val.is_none_or(|v| v.is_undefined() || v.is_null()) {
            return Ok(Vec::new());
        }

        let cmds_arr = Local::<deno_core::v8::Array>::try_from(val.unwrap()).map_err(|_| {
            ExtractError::InvalidField {
                field: "commands".to_string(),
                reason: "expected an array".into(),
            }
        })?;

        let mut commands = Vec::new();
        for i in 0..cmds_arr.length() {
            let cmd_obj = get_array_obj(scope, cmds_arr, i, "commands")?;

            let name = {
                let k = v8_str(scope, "name")?;
                let v = cmd_obj
                    .get(scope, k.into())
                    .filter(|v| !v.is_undefined() && !v.is_null())
                    .ok_or_else(|| ExtractError::InvalidField {
                        field: format!("commands[{i}].name"),
                        reason: "missing required field".into(),
                    })?;
                v.to_rust_string_lossy(scope)
            };

            let description = optional_string(scope, cmd_obj, "description");

            commands.push(LoadedCommand { name, description });
        }

        Ok(commands)
    }

    // -- Array helper --

    /// Get an object from an array index, with context in the error.
    fn get_array_obj<'s>(
        scope: &mut PinnedRef<'s, HandleScope<'_>>,
        arr: Local<'s, deno_core::v8::Array>,
        index: u32,
        context: &str,
    ) -> Result<Local<'s, Object>, ExtractError> {
        let val = arr
            .get_index(scope, index)
            .unwrap_or_else(|| deno_core::v8::undefined(scope).into());
        Local::<Object>::try_from(val).map_err(|_| ExtractError::InvalidField {
            field: format!("{context}[{index}]"),
            reason: "expected an object".into(),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{JsRuntime, RuntimeOptions};
    use url::Url;

    /// Helper: load a TS module, extract its manifest.
    ///
    /// Transpiles `ts_source`, loads it into a fresh `JsRuntime`, gets the
    /// module namespace, and calls `extract_manifest`.
    fn extract_from_ts(ts_source: &str) -> Result<LoadedExtension, ManifestError> {
        let js = crate::transpile::transpile(&Url::parse("file:///test.ts").unwrap(), ts_source)
            .map_err(ManifestError::V8)?;

        let local = tokio::task::LocalSet::new();
        local.block_on(
            &tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            async {
                let mut rt = JsRuntime::new(RuntimeOptions::default());
                let specifier = Url::parse("file:///test.ts").unwrap();

                let mod_id = rt
                    .load_main_es_module_from_code(&specifier, js)
                    .await
                    .expect("module load should succeed");
                let _ = rt.mod_evaluate(mod_id);
                rt.run_event_loop(Default::default())
                    .await
                    .expect("event loop should complete");

                let namespace = rt
                    .get_module_namespace(mod_id)
                    .expect("should get namespace");

                deno_core::scope!(scope, rt);
                let ns = deno_core::v8::Local::<deno_core::v8::Object>::new(scope, namespace);
                extract_manifest(scope, ns)
            },
        )
    }

    // -- Test: minimal manifest with just a name --

    #[test]
    fn minimal_manifest_name_only() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "test-extension",
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.name, "test-extension");
        assert!(ext.version.is_none());
        assert!(ext.tools.is_empty());
        assert!(ext.commands.is_empty());
    }

    // -- Test: manifest with version --

    #[test]
    fn manifest_with_version() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "versioned",
                version: "2.1.0",
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.name, "versioned");
        assert_eq!(ext.version.as_deref(), Some("2.1.0"));
    }

    // -- Test: no default export --

    #[test]
    fn no_default_export() {
        let err = extract_from_ts(
            r#"
            export function foo() { return 1; }
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::NoDefaultExport));
    }

    // -- Test: default export is not an object --

    #[test]
    fn default_export_is_string() {
        let err = extract_from_ts(
            r#"
            export default "not an object";
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::DefaultExportNotObject));
    }

    // -- Test: missing name field --

    #[test]
    fn missing_name_field() {
        let err = extract_from_ts(
            r#"
            export default {
                tools: [],
            };
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::MissingField(f) if f == "name"));
    }

    // -- Test: one tool with parameters --

    #[test]
    fn one_tool_with_parameters() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "tool-test",
                tools: [{
                    name: "search",
                    description: "Search for something",
                    risk: "read" as const,
                    parameters: {
                        query: { type: "string", description: "Search query", required: true },
                        limit: { type: "number", description: "Max results", required: false },
                    },
                    execute: async (args: any) => { return { output: "ok" }; },
                }],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.tools.len(), 1);
        let tool = &ext.tools[0];
        assert_eq!(tool.name, "search");
        assert_eq!(tool.description, "Search for something");
        assert_eq!(tool.risk, ToolRisk::Read);

        assert_eq!(tool.parameters.len(), 2);

        let query = &tool.parameters["query"];
        assert_eq!(query.param_type, "string");
        assert_eq!(query.description.as_deref(), Some("Search query"));
        assert!(query.required);

        let limit = &tool.parameters["limit"];
        assert_eq!(limit.param_type, "number");
        assert_eq!(limit.description.as_deref(), Some("Max results"));
        assert!(!limit.required);
    }

    // -- Test: tool with write risk --

    #[test]
    fn tool_with_write_risk() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "risk-test",
                tools: [{
                    name: "modify",
                    description: "Modifies things",
                    risk: "write" as const,
                    parameters: {},
                    execute: async () => { return { output: "done" }; },
                }],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.tools[0].risk, ToolRisk::Write);
    }

    // -- Test: tool with destructive risk --

    #[test]
    fn tool_with_destructive_risk() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "risk-test",
                tools: [{
                    name: "nuke",
                    description: "Destroys everything",
                    risk: "destructive" as const,
                    parameters: {},
                    execute: async () => { return { output: "boom" }; },
                }],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.tools[0].risk, ToolRisk::Destructive);
    }

    // -- Test: tool with invalid risk --

    #[test]
    fn tool_with_invalid_risk() {
        let err = extract_from_ts(
            r#"
            export default {
                name: "bad-risk",
                tools: [{
                    name: "bad",
                    description: "Bad risk",
                    risk: "nuclear" as const,
                    parameters: {},
                    execute: async () => { return { output: "" }; },
                }],
            };
            "#,
        )
        .unwrap_err();

        assert!(
            matches!(err, ManifestError::ToolInvalidField { index: 0, field, .. } if field == "risk")
        );
    }

    // -- Test: tool missing required field --

    #[test]
    fn tool_missing_name() {
        let err = extract_from_ts(
            r#"
            export default {
                name: "no-tool-name",
                tools: [{
                    description: "No name",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => { return { output: "" }; },
                }],
            };
            "#,
        )
        .unwrap_err();

        assert!(
            matches!(err, ManifestError::ToolMissingField { index: 0, field } if field == "name")
        );
    }

    // -- Test: multiple tools --

    #[test]
    fn multiple_tools() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "multi-tool",
                tools: [
                    {
                        name: "read-thing",
                        description: "Reads a thing",
                        risk: "read" as const,
                        parameters: {},
                        execute: async () => { return { output: "read" }; },
                    },
                    {
                        name: "write-thing",
                        description: "Writes a thing",
                        risk: "write" as const,
                        parameters: {},
                        execute: async () => { return { output: "written" }; },
                    },
                ],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.tools.len(), 2);
        assert_eq!(ext.tools[0].name, "read-thing");
        assert_eq!(ext.tools[1].name, "write-thing");
    }

    // -- Test: tool without parameters --

    #[test]
    fn tool_without_parameters() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "no-params",
                tools: [{
                    name: "ping",
                    description: "Returns pong",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => { return { output: "pong" }; },
                }],
            };
            "#,
        )
        .unwrap();

        assert!(ext.tools[0].parameters.is_empty());
    }

    // -- Test: hooks --

    #[test]
    fn hooks_extraction() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "hooked",
                hooks: {
                    onLoad: async () => {},
                    onToolCall: async (name: string, args: any) => {},
                    onToolResult: async (name: string, result: any) => {},
                    onBeforeModel: async (msgs: any) => {},
                },
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.hooks.on_load.as_deref(), Some("onLoad"));
        assert_eq!(ext.hooks.on_tool_call.as_deref(), Some("onToolCall"));
        assert_eq!(ext.hooks.on_tool_result.as_deref(), Some("onToolResult"));
        assert_eq!(ext.hooks.on_before_model.as_deref(), Some("onBeforeModel"));
    }

    // -- Test: partial hooks --

    #[test]
    fn partial_hooks() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "partial-hooks",
                hooks: {
                    onToolCall: async (name: string, args: any) => {},
                },
            };
            "#,
        )
        .unwrap();

        assert!(ext.hooks.on_load.is_none());
        assert_eq!(ext.hooks.on_tool_call.as_deref(), Some("onToolCall"));
        assert!(ext.hooks.on_tool_result.is_none());
        assert!(ext.hooks.on_before_model.is_none());
    }

    // -- Test: no hooks --

    #[test]
    fn no_hooks() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "no-hooks",
            };
            "#,
        )
        .unwrap();

        assert!(ext.hooks.on_load.is_none());
        assert!(ext.hooks.on_tool_call.is_none());
        assert!(ext.hooks.on_tool_result.is_none());
        assert!(ext.hooks.on_before_model.is_none());
    }

    // -- Test: commands --

    #[test]
    fn commands_extraction() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "cmd-test",
                commands: [
                    {
                        name: "search",
                        description: "Search for something",
                        handler: (args: string) => {},
                    },
                    {
                        name: "deploy",
                        handler: (args: string) => {},
                    },
                ],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.commands.len(), 2);
        assert_eq!(ext.commands[0].name, "search");
        assert_eq!(
            ext.commands[0].description.as_deref(),
            Some("Search for something")
        );
        assert_eq!(ext.commands[1].name, "deploy");
        assert!(ext.commands[1].description.is_none());
    }

    // -- Test: command missing name --

    #[test]
    fn command_missing_name() {
        let err = extract_from_ts(
            r#"
            export default {
                name: "bad-cmd",
                commands: [{
                    handler: (args: string) => {},
                }],
            };
            "#,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            ManifestError::InvalidField { ref field, .. } if field.starts_with("commands[0].name")
        ));
    }

    // -- Test: full manifest (tools + hooks + commands) --

    #[test]
    fn full_manifest() {
        let ext = extract_from_ts(
            r#"
            export default {
                name: "full-extension",
                version: "1.2.3",
                tools: [{
                    name: "greet",
                    description: "Greet someone",
                    risk: "read" as const,
                    parameters: {
                        name: { type: "string", description: "Who to greet", required: true },
                    },
                    execute: async (args: any) => {
                        return { output: "Hello!" };
                    },
                }],
                hooks: {
                    onLoad: async () => {},
                    onToolCall: async (name: string, args: any) => {},
                },
                commands: [{
                    name: "hello",
                    description: "Say hello",
                    handler: (args: string) => {},
                }],
            };
            "#,
        )
        .unwrap();

        assert_eq!(ext.name, "full-extension");
        assert_eq!(ext.version.as_deref(), Some("1.2.3"));

        assert_eq!(ext.tools.len(), 1);
        assert_eq!(ext.tools[0].name, "greet");
        assert_eq!(ext.tools[0].parameters.len(), 1);

        assert!(ext.hooks.on_load.is_some());
        assert!(ext.hooks.on_tool_call.is_some());
        assert!(ext.hooks.on_tool_result.is_none());

        assert_eq!(ext.commands.len(), 1);
        assert_eq!(ext.commands[0].name, "hello");
    }
}
