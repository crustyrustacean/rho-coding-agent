//! Error types for rho-tools.

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during tool execution.
#[derive(Debug, Error)]
pub enum ToolError {
    /// A required tool argument was not provided.
    #[error("missing required argument `{name}`")]
    MissingArgument {
        /// The name of the missing argument.
        name: String,
    },

    /// A file or I/O operation failed.
    #[error("I/O error on `{path}`: {source}")]
    FileSystem {
        /// The path that caused the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// An external command exited with a non-zero status.
    #[error("command `{command}` failed (exit code {exit_code}): {stderr}")]
    CommandFailed {
        /// The command that was executed.
        command: String,
        /// The exit code.
        exit_code: i32,
        /// The standard error output.
        stderr: String,
    },

    /// An external command could not be spawned (before any output).
    #[error("failed to spawn command `{command}`: {source}")]
    CommandSpawn {
        /// The command that could not be spawned.
        command: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// A tool argument specified a path outside the allowed working directory.
    #[error("path `{path}` is outside the working directory")]
    WorkingDirectoryEscape {
        /// The path that escaped the working directory.
        path: PathBuf,
    },

    /// A tool operation was cancelled by the user or cancellation token.
    #[error("tool cancelled")]
    Cancelled,

    /// An HTTP API request failed (transport-level error).
    #[error("HTTP request failed: {source}")]
    Http {
        /// The underlying reqwest error.
        source: reqwest::Error,
    },

    /// An external API returned a non-success HTTP status.
    #[error("API error (HTTP {status}): {message}")]
    ApiError {
        /// The HTTP status code.
        status: u16,
        /// The response body (may be truncated).
        message: String,
    },

    /// JSON deserialization of an API response failed.
    #[error("JSON parsing failed: {source}")]
    Json {
        /// The underlying `serde_json` error.
        source: serde_json::Error,
    },

    /// A sandbox violation was detected.
    #[error("sandbox violation: {0}")]
    SandboxViolation(String),

    /// A shell command exceeded its configured timeout.
    #[error("command timed out after {duration_ms}ms")]
    Timeout {
        /// The timeout duration in milliseconds.
        duration_ms: u64,
    },

    /// A generic tool error (e.g., misconfiguration, unavailable resource).
    #[error("{message}")]
    Internal {
        /// Human-readable error description.
        message: String,
    },
}

/// A specialised `Result` type for rho-tools operations.
pub type ToolResult<T> = std::result::Result<T, ToolError>;

/// Convert [`ToolError`] to [`rho_core::RhoError`] via the `Tool` variant.
///
/// This bridges tool-specific errors into the top-level error type so that
/// `?` works in trait implementations that return `rho_core::Result`.
impl From<ToolError> for rho_core::RhoError {
    fn from(error: ToolError) -> Self {
        rho_core::RhoError::Tool(error.to_string())
    }
}
