//! Wire-format types for tool definitions sent in [`ChatRequest`].
//!
//! [`ToolSchema`] is the *description* the model sees in a request. It is distinct
//! from the [`Tool`] trait, which is the *implementation* rho executes.
//!
//! [`ChatRequest`]: crate::request::ChatRequest
//! [`Tool`]: crate::tool::Tool

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool definition sent to the model API.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    /// The tool type — always `"function"`.
    #[serde(rename = "type")]
    pub tool_type: String,
    /// The function signature exposed to the model.
    pub function: ToolSchemaFunction,
}

/// The function portion of a [`ToolSchema`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchemaFunction {
    /// The function name.
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema describing the function's parameters.
    pub parameters: Value,
}

impl ToolSchema {
    /// Convenience constructor for a function-type tool schema.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            tool_type: "function".to_owned(),
            function: ToolSchemaFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}
