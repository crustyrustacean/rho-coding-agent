//! Error types for session management operations.

use thiserror::Error;

/// Errors that can occur during session operations.
#[derive(Debug, Error)]
pub enum SessionError {
    /// An entry was not found in the session tree.
    ///
    /// Returned by tree navigation methods (e.g., `branch_to`) when the
    /// target entry ID does not exist in the session.
    #[error("entry not found: {0}")]
    EntryNotFound(String),

    /// A persistence operation failed.
    #[error("persistence error: {0}")]
    Persistence(String),
}

impl SessionError {
    /// Create an `EntryNotFound` error.
    pub fn entry_not_found(id: impl Into<String>) -> Self {
        SessionError::EntryNotFound(id.into())
    }

    /// Create a `Persistence` error.
    pub fn persistence_error(message: impl Into<String>) -> Self {
        SessionError::Persistence(message.into())
    }
}

/// A specialised `Result` type for session operations.
pub type SessionResult<T> = std::result::Result<T, SessionError>;

// Backward compatibility: convert SessionError to RhoError
impl From<SessionError> for crate::error::RhoError {
    fn from(error: SessionError) -> Self {
        crate::error::RhoError::Session(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_not_found() {
        let error = SessionError::entry_not_found("entry-123");
        assert!(matches!(error, SessionError::EntryNotFound(_)));
        assert_eq!(error.to_string(), "entry not found: entry-123");
    }

    #[test]
    fn test_persistence_error() {
        let error = SessionError::persistence_error("failed to write file");
        assert!(matches!(error, SessionError::Persistence(_)));
        assert_eq!(error.to_string(), "persistence error: failed to write file");
    }

    #[test]
    fn test_session_result_ok() {
        let result: SessionResult<String> = Ok("success".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn test_session_result_err() {
        let result: SessionResult<String> = Err(SessionError::EntryNotFound("test".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_display_formats() {
        assert_eq!(
            SessionError::EntryNotFound("abc123".to_string()).to_string(),
            "entry not found: abc123"
        );
        assert_eq!(
            SessionError::Persistence("disk full".to_string()).to_string(),
            "persistence error: disk full"
        );
    }

    #[test]
    fn test_error_debug() {
        let error = SessionError::EntryNotFound("debug-test".to_string());
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("EntryNotFound"));
        assert!(debug_str.contains("debug-test"));
    }

    #[test]
    fn test_factory_methods() {
        let error1 = SessionError::entry_not_found("test-id");
        let error2 = SessionError::EntryNotFound("test-id".to_string());
        assert_eq!(error1.to_string(), error2.to_string());

        let error3 = SessionError::persistence_error("test error");
        let error4 = SessionError::Persistence("test error".to_string());
        assert_eq!(error3.to_string(), error4.to_string());
    }
}
