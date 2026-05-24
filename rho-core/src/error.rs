//! Error types for rho-core.

use crate::agent::AgentError;
use crate::client::error::ClientError;
use crate::sandbox::SandboxError;
use crate::session::error::SessionError;
use thiserror::Error;

/// A unified error type for the top-level agent loop.
///
/// Each variant wraps a domain-specific error. This serves as a thin boundary
/// enum for code that needs to handle errors from multiple domains simultaneously.
#[derive(Debug, Error)]
pub enum RhoError {
    /// Client/API errors (HTTP, JSON, retry budget)
    #[error(transparent)]
    Client(ClientError),

    /// Agent loop errors (iteration limit, cancellation, protocol violation)
    #[error(transparent)]
    Agent(AgentError),

    /// Session management errors (entry not found, persistence)
    #[error(transparent)]
    Session(SessionError),

    /// Sandbox/file path errors
    #[error(transparent)]
    Sandbox(SandboxError),

    /// The agent loop was asked to invoke a tool that is not registered.
    ///
    /// This is a registry concern that spans domains, so it stays at the top level.
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// Transient errors were retried until the retry budget was exhausted.
    ///
    /// This spans multiple domains (client, tools, etc.), so it stays at the top level.
    #[error("retry budget exhausted after {0} attempts: {1}")]
    RetryBudgetExhausted(u32, Box<RhoError>),

    /// A tool execution error that could not be classified into a more specific domain.
    #[error("tool error: {0}")]
    Tool(String),
}

impl RhoError {
    /// Returns `true` if this error is transient and the operation may be retried.
    ///
    /// This delegates to the [`Retryable`](crate::client::error::Retryable) trait
    /// implementation for [`ClientError`]. All other error types are considered
    /// non-retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            RhoError::Client(e) => crate::client::error::Retryable::is_retryable(e),
            _ => false,
        }
    }
}

// Backward compatibility: convert from old RhoError variants to new structure
impl From<reqwest::Error> for RhoError {
    fn from(error: reqwest::Error) -> Self {
        RhoError::Client(ClientError::Http(error))
    }
}

impl From<serde_json::Error> for RhoError {
    fn from(error: serde_json::Error) -> Self {
        RhoError::Client(ClientError::Json(error))
    }
}

/// A specialised `Result` type for rho-core operations.
pub type Result<T> = std::result::Result<T, RhoError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::error::ClientError;

    #[test]
    fn test_thin_boundary_enum_wraps_client_error() {
        let rho_error = RhoError::Client(ClientError::http_error(
            500,
            "Internal Server Error".to_string(),
        ));

        assert!(matches!(rho_error, RhoError::Client(_)));
        assert!(rho_error.to_string().contains("500"));
        assert!(rho_error.is_retryable());
    }

    #[test]
    fn test_thin_boundary_enum_wraps_agent_error() {
        let agent_error = AgentError::max_iterations_exceeded(100);
        let rho_error = RhoError::Agent(agent_error);

        assert!(matches!(rho_error, RhoError::Agent(_)));
        assert!(rho_error.to_string().contains("100"));
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_thin_boundary_enum_wraps_session_error() {
        let session_error = SessionError::entry_not_found("test-id");
        let rho_error = RhoError::Session(session_error);

        assert!(matches!(rho_error, RhoError::Session(_)));
        assert!(rho_error.to_string().contains("test-id"));
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_thin_boundary_enum_wraps_sandbox_error() {
        let sandbox_error = SandboxError::PathEscape {
            path: "/etc/passwd".to_string(),
        };
        let rho_error = RhoError::Sandbox(sandbox_error);

        assert!(matches!(rho_error, RhoError::Sandbox(_)));
        assert!(rho_error.to_string().contains("outside the sandbox root"));
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_tool_not_found_stays_in_boundary() {
        let rho_error = RhoError::ToolNotFound("my_tool".to_string());

        assert!(matches!(rho_error, RhoError::ToolNotFound(_)));
        assert!(rho_error.to_string().contains("my_tool"));
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_is_retryable_delegates_to_client_retryable() {
        // Retryable client errors should be retryable
        let retryable_error = ClientError::http_error(503, "Service Unavailable".to_string());
        let rho_error = RhoError::Client(retryable_error);
        assert!(rho_error.is_retryable());

        // Non-retryable client errors should not be retryable
        let non_retryable_error = ClientError::http_error(404, "Not Found".to_string());
        let rho_error = RhoError::Client(non_retryable_error);
        assert!(!rho_error.is_retryable());

        // Agent errors should not be retryable
        let agent_error = AgentError::Cancelled;
        let rho_error = RhoError::Agent(agent_error);
        assert!(!rho_error.is_retryable());

        // Session errors should not be retryable
        let session_error = SessionError::entry_not_found("test-id");
        let rho_error = RhoError::Session(session_error);
        assert!(!rho_error.is_retryable());

        // Sandbox errors should not be retryable
        let sandbox_error = SandboxError::PathEscape {
            path: "/escape".to_string(),
        };
        let rho_error = RhoError::Sandbox(sandbox_error);
        assert!(!rho_error.is_retryable());

        // ToolNotFound should not be retryable
        let rho_error = RhoError::ToolNotFound("tool".to_string());
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_backward_compat_from_reqwest_error() {
        // Test the From<reqwest::Error> implementation
        // We create an HTTP error using the ClientError factory, then
        // wrap it in RhoError to test the From impl
        let client_error = ClientError::http_error(400, "Bad Request".to_string());
        let rho_error = RhoError::Client(client_error);

        assert!(matches!(rho_error, RhoError::Client(_)));
        assert!(!rho_error.is_retryable());
    }

    #[test]
    fn test_backward_compat_from_json_error() {
        let json_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let rho_error: RhoError = json_error.into();

        assert!(matches!(rho_error, RhoError::Client(ClientError::Json(_))));
    }

    #[test]
    fn test_debug_format_preserves_structure() {
        let client_error = ClientError::http_error(500, "error".to_string());
        let rho_error = RhoError::Client(client_error);

        let debug_str = format!("{rho_error:?}");
        assert!(debug_str.contains("Client"));
    }
}
