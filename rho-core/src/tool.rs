//! The [`Tool`] trait, supporting types, and [`ToolRegistry`].

use crate::error::Result;
use crate::message::ModelToolCall;
use crate::newtypes::ToolName;
use crate::schema::ToolSchema;
use async_trait::async_trait;
use std::sync::Arc;

// ── CancellationToken ─────────────────────────────────────────────────────────

/// A cancellation signal passed to [`Tool::execute`].
///
/// Cheap to clone (Arc-backed). Tools should check [`is_cancelled`] at I/O
/// boundaries and return early when set.
///
/// [`is_cancelled`]: CancellationToken::is_cancelled
#[derive(Clone, Default)]
pub struct CancellationToken {
    /// Shared flag set to `true` when cancellation is signalled.
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl CancellationToken {
    /// Create a new, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Signal cancellation.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns `true` if cancellation has been signalled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }
}

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

/// The immediate result of a tool execution.
#[derive(Clone, Debug)]
pub struct ToolResult {
    /// The text output of the tool.
    pub output: String,
    /// `true` if the tool reported an error (e.g. non-zero exit code).
    pub is_error: bool,
}

impl ToolResult {
    /// Create a successful result.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    /// Create an error result.
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
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
    /// Implementations should return a `&'static str` literal so the description
    /// can be used without lifetime complications.
    fn description(&self) -> &'static str;

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
    pub fn register(&mut self, tool: Box<dyn Tool>) {
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
            ToolOutcome::Streamed(_) => Err(crate::error::RhoError::Unexpected(anyhow::anyhow!(
                "streaming tool output is not yet supported"
            ))),
        }
    }
}
