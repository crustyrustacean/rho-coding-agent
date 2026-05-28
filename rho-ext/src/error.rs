//! Error types for the rho extension runtime.

use thiserror::Error;

/// Errors that can occur during extension loading or execution.
#[derive(Debug, Error)]
pub enum ExtensionError {
    /// TypeScript transpilation failed.
    #[error("transpilation failed: {0}")]
    Transpile(String),

    /// V8 module loading failed.
    #[error("module load failed: {0}")]
    ModuleLoad(String),

    /// A requested export was not found in the module.
    #[error("export not found: {0}")]
    ExportNotFound(String),

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
