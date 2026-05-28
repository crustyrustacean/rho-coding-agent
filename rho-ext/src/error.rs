//! Error types for the rho extension runtime.

use thiserror::Error;

use crate::manifest::ManifestError;

/// Errors that can occur during extension loading or execution.
#[derive(Debug, Error)]
pub enum ExtensionError {
    /// TypeScript transpilation failed.
    #[error("transpilation failed: {0}")]
    Transpile(String),

    /// V8 module loading failed.
    #[error("module load failed: {0}")]
    ModuleLoad(String),

    /// Manifest extraction or validation failed.
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),

    /// A tool was not found in the extension.
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// A tool is missing its `execute` function.
    #[error("tool '{0}' has no execute function")]
    ToolMissingExecute(String),

    /// A hook was not found in the extension.
    #[error("hook not found: {0}")]
    HookNotFound(String),

    /// A command was not found in the extension.
    #[error("command not found: {0}")]
    CommandNotFound(String),

    /// A command is missing its `handler` function.
    #[error("command '{0}' has no handler function")]
    CommandMissingHandler(String),

    /// The extension thread has shut down (sender dropped).
    #[error("extension runtime shut down")]
    RuntimeShutdown,

    /// Calling an extension function produced a JS error.
    #[error("extension execution error: {0}")]
    Execution(String),

    /// A timeout occurred while waiting for the extension to respond.
    #[error("extension call timed out after {0:?}")]
    Timeout(std::time::Duration),
}
