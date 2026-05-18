//! Error types for the agent loop.

use thiserror::Error;

/// Errors that can occur during agent loop operations.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The agent loop exceeded its configured iteration limit.
    #[error("agent loop exceeded maximum iterations ({0})")]
    MaxIterationsExceeded(u32),

    /// The agent loop was cancelled by the user or a cancellation token.
    #[error("cancelled")]
    Cancelled,

    /// The model API returned a response that violates the expected protocol.
    ///
    /// For example, the model returned an empty `tool_calls` array or a
    /// streaming outcome that is not yet supported.
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),
}

impl AgentError {
    /// Create a `MaxIterationsExceeded` error.
    pub fn max_iterations_exceeded(limit: u32) -> Self {
        AgentError::MaxIterationsExceeded(limit)
    }

    /// Create a `ProtocolViolation` error.
    pub fn protocol_violation(message: impl Into<String>) -> Self {
        AgentError::ProtocolViolation(message.into())
    }
}

/// A specialised `Result` type for agent operations.
pub type AgentResult<T> = std::result::Result<T, AgentError>;

// Backward compatibility: convert AgentError to RhoError
impl From<AgentError> for crate::error::RhoError {
    fn from(error: AgentError) -> Self {
        crate::error::RhoError::Agent(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_iterations_exceeded() {
        let error = AgentError::max_iterations_exceeded(100);
        assert!(matches!(error, AgentError::MaxIterationsExceeded(100)));
        assert_eq!(
            error.to_string(),
            "agent loop exceeded maximum iterations (100)"
        );
    }

    #[test]
    fn test_cancelled() {
        let error = AgentError::Cancelled;
        assert!(matches!(error, AgentError::Cancelled));
        assert_eq!(error.to_string(), "cancelled");
    }

    #[test]
    fn test_protocol_violation() {
        let error = AgentError::protocol_violation("invalid tool call format");
        assert!(matches!(error, AgentError::ProtocolViolation(_)));
        assert_eq!(
            error.to_string(),
            "protocol violation: invalid tool call format"
        );
    }

    #[test]
    fn test_agent_result_ok() {
        let result: AgentResult<String> = Ok("success".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_result_err() {
        let result: AgentResult<String> = Err(AgentError::Cancelled);
        assert!(result.is_err());
    }

    #[test]
    fn test_display_formats() {
        assert_eq!(
            AgentError::MaxIterationsExceeded(50).to_string(),
            "agent loop exceeded maximum iterations (50)"
        );
        assert_eq!(AgentError::Cancelled.to_string(), "cancelled");
        assert_eq!(
            AgentError::ProtocolViolation("test".to_string()).to_string(),
            "protocol violation: test"
        );
    }

    #[test]
    fn test_error_debug() {
        let error = AgentError::ProtocolViolation("debug test".to_string());
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("ProtocolViolation"));
        assert!(debug_str.contains("debug test"));
    }
}
