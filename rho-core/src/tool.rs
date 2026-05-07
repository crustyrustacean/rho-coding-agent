//! The [`Tool`] trait, supporting types, and [`ToolRegistry`].

use crate::error::Result;
use crate::message::ModelToolCall;
use crate::newtypes::ToolName;
use crate::schema::ToolSchema;
use async_trait::async_trait;

// ── CancellationToken ─────────────────────────────────────────────────────────

/// A cancellation signal passed to [`Tool::execute`].
///
/// Re-exported from `tokio_util::sync::CancellationToken`. Provides both
/// synchronous [`is_cancelled`] polling and an `await`-able [`cancelled()`]
/// future that integrates with `tokio::select!`.
///
/// [`is_cancelled`]: CancellationToken::is_cancelled
/// [`cancelled()`]: tokio_util::sync::CancellationToken::cancelled
pub use tokio_util::sync::CancellationToken;

// ── ToolRisk ──────────────────────────────────────────────────────────────────

/// The potential impact of a tool's execution.
///
/// Used by the approval policy (Phase 1b) to decide whether to require
/// human confirmation before running a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRisk {
    /// The tool only reads data; it cannot modify state.
    Read,
    /// The tool may create or modify files.
    Write,
    /// The tool may execute commands, delete data, or cause irreversible effects.
    Destructive,
}

// ── ToolResult / ToolOutcome ──────────────────────────────────────────────────

/// Structured detail attached to a tool result.
///
/// This enum carries tool-specific payloads that travel alongside the
/// text `output` but are **not** sent to the model. Tools and extensions
/// can read `details` to access richer information than what the LLM sees.
///
/// Phase 2.5 ships [`None`](ToolResultDetails::None) and
/// [`FullOutput`](ToolResultDetails::FullOutput). Phase 3 adds
/// [`Diagnostics`](ToolResultDetails::Diagnostics) for structured
/// compiler output from `cargo check` / `cargo clippy`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ToolResultDetails {
    /// No structured detail attached (the default).
    #[default]
    None,
    /// The tool result was truncated before entering the LLM context;
    /// this variant preserves the full, un-truncated output.
    FullOutput {
        /// Original size of the output in bytes, before truncation.
        original_size: usize,
        /// The complete, un-truncated content.
        content: String,
    },
    /// Structured compiler diagnostics from `cargo check` or `cargo clippy`.
    ///
    /// The `Vec` contains only workspace-local diagnostics (dependency noise
    /// is filtered out). Each [`Diagnostic`] carries the error code, message,
    /// source spans, and machine-applicable suggestions.
    ///
    /// [`Diagnostic`]: crate::Diagnostic
    Diagnostics(Vec<crate::diagnostic::Diagnostic>),
}

/// The immediate result of a tool execution.
#[derive(Clone, Debug)]
pub struct ToolResult {
    /// The text output of the tool.
    pub output: String,
    /// `true` if the tool reported an error (e.g. non-zero exit code).
    pub is_error: bool,
    /// Structured detail not sent to the model but available to tools
    /// and extensions. Defaults to [`ToolResultDetails::None`].
    pub details: ToolResultDetails,
}

impl ToolResult {
    /// Create a successful result with no structured detail.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            details: ToolResultDetails::None,
        }
    }

    /// Create an error result with no structured detail.
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            details: ToolResultDetails::None,
        }
    }
}

/// A single chunk of incremental tool output (Phase 4).
#[derive(Clone, Debug)]
pub struct ToolChunk {
    /// A segment of tool output.
    pub text: String,
}

/// The result of a [`Tool::execute`] call.
///
/// Phase 1a tools only emit [`Immediate`]. [`Streamed`] is declared now so Phase 4's
/// TUI streaming is a new variant rather than a workspace-wide signature change.
///
/// [`Immediate`]: ToolOutcome::Immediate
/// [`Streamed`]: ToolOutcome::Streamed
pub enum ToolOutcome {
    /// Tool completed immediately.
    Immediate(ToolResult),
    /// Tool produces output incrementally. Not exercised until Phase 4.
    Streamed(tokio::sync::mpsc::Receiver<ToolChunk>),
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// The interface all rho tools must implement.
///
/// # Dyn-compatibility
///
/// `#[async_trait]` is required because [`ToolRegistry`] stores `Box<dyn Tool>`.
/// Native async-fn-in-traits (stable in 1.75) are not dyn-compatible.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's registered name.
    fn name(&self) -> ToolName;

    /// Human-readable description shown to the model.
    ///
    /// Implementations typically return a `&'static str` literal, but the
    /// return type is `&str` to allow heap-allocated descriptions from
    /// dynamically loaded extensions in future phases.
    fn description(&self) -> &str;

    /// JSON Schema object describing this tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Risk classification, used by the approval policy (Phase 1b).
    fn risk(&self) -> ToolRisk;

    /// Execute the tool.
    ///
    /// Tools should check `cancel.is_cancelled()` at I/O boundaries.
    /// Tool-level errors (non-zero exit, etc.) should be returned as
    /// [`ToolResult::error`] inside a successful [`ToolOutcome::Immediate`],
    /// not as an `Err`.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome>;
}

// ── ToolRegistry ──────────────────────────────────────────────────────────────

/// Maps tool names to [`Tool`] implementations.
#[derive(Default)]
pub struct ToolRegistry {
    /// Registered tool implementations.
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool.
    ///
    /// # Panics
    ///
    /// Panics if a tool with the same name is already registered. Tool
    /// registration happens at startup, so duplicate names are a
    /// programming error that should fail fast rather than silently
    /// shadowing the earlier registration.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name();
        assert!(
            self.get_by_name(&name).is_none(),
            "duplicate tool name: '{}'",
            &*name
        );
        self.tools.push(tool);
    }

    /// All registered tools.
    pub fn list(&self) -> &[Box<dyn Tool>] {
        &self.tools
    }

    /// Look up a tool by name.
    pub fn get_by_name(&self, name: &ToolName) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| *t.name() == **name)
            .map(std::convert::AsRef::as_ref)
    }

    /// Produce tool schemas for inclusion in a [`ChatRequest`].
    ///
    /// [`ChatRequest`]: crate::request::ChatRequest
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .iter()
            .map(|t| {
                ToolSchema::function(t.name().to_string(), t.description(), t.parameters_schema())
            })
            .collect()
    }

    /// Execute a model-issued tool call.
    ///
    /// # Errors
    ///
    /// - [`RhoError::ToolNotFound`] — no tool with the given name is registered
    /// - [`RhoError::Json`] — arguments could not be parsed
    /// - Any error returned by [`Tool::execute`]
    ///
    /// [`RhoError::ToolNotFound`]: crate::error::RhoError::ToolNotFound
    /// [`RhoError::Json`]: crate::error::RhoError::Json
    pub async fn execute(
        &self,
        call: &ModelToolCall,
        cancel: CancellationToken,
    ) -> Result<ToolResult> {
        let name = call.function.name.clone();
        let tool = self
            .get_by_name(&name)
            .ok_or_else(|| crate::error::RhoError::ToolNotFound(name.to_string()))?;

        let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)?;
        let outcome = tool.execute(arguments, cancel).await?;

        match outcome {
            ToolOutcome::Immediate(result) => Ok(result),
            ToolOutcome::Streamed(_) => Err(crate::error::RhoError::ProtocolViolation(
                "streaming tool output is not yet supported".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial tool for registry tests.
    struct StubTool {
        name: &'static str,
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> ToolName {
            ToolName::from(self.name)
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn risk(&self) -> ToolRisk {
            ToolRisk::Read
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _cancel: CancellationToken,
        ) -> Result<ToolOutcome> {
            Ok(ToolOutcome::Immediate(ToolResult::success("stub")))
        }
    }

    #[test]
    fn tool_result_success_has_no_details() {
        let result = ToolResult::success("hello");
        assert!(!result.is_error);
        assert_eq!(result.output, "hello");
        assert_eq!(result.details, ToolResultDetails::None);
    }

    #[test]
    fn tool_result_error_has_no_details() {
        let result = ToolResult::error("boom");
        assert!(result.is_error);
        assert_eq!(result.output, "boom");
        assert_eq!(result.details, ToolResultDetails::None);
    }

    #[test]
    fn tool_result_details_default_is_none() {
        assert_eq!(ToolResultDetails::default(), ToolResultDetails::None);
    }

    #[test]
    fn tool_result_details_full_output_equality() {
        let a = ToolResultDetails::FullOutput {
            original_size: 100,
            content: "x".repeat(100),
        };
        let b = ToolResultDetails::FullOutput {
            original_size: 100,
            content: "x".repeat(100),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn register_duplicate_tool_name_panics() {
        let result = std::panic::catch_unwind(|| {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(StubTool { name: "my_tool" }));
            reg.register(Box::new(StubTool { name: "my_tool" }));
        });
        assert!(
            result.is_err(),
            "registering duplicate tool name should panic"
        );
    }

    #[test]
    fn register_distinct_tool_names_succeeds() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(StubTool { name: "tool_a" }));
        registry.register(Box::new(StubTool { name: "tool_b" }));
        assert_eq!(registry.list().len(), 2);
    }
}
