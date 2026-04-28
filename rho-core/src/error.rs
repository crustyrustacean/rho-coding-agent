//! Error types for rho-core.

use thiserror::Error;

/// Errors that can occur in rho-core operations.
#[derive(Debug, Error)]
pub enum RhoError {
    /// An HTTP request to the model API failed.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    /// A response body could not be parsed as JSON.
    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    /// The agent loop was asked to invoke a tool that is not registered.
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// The agent loop exceeded its configured iteration limit.
    #[error("agent loop exceeded maximum iterations ({0})")]
    MaxIterationsExceeded(u32),

    /// Transient errors were retried until the retry budget was exhausted.
    #[error("retry budget exhausted after {0} attempts")]
    RetryBudgetExhausted(u32),

    /// An unexpected error occurred.
    #[error(transparent)]
    Unexpected(#[from] anyhow::Error),
}

impl RhoError {
    /// Returns `true` if this error is transient and the operation may be retried.
    ///
    /// Retryable: HTTP 429, 500, 502, 503, 504, or network-level errors (no status).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            RhoError::Http(e) => e
                .status()
                .is_none_or(|s| matches!(s.as_u16(), 429 | 500 | 502 | 503 | 504)),
            _ => false,
        }
    }
}

/// A specialised `Result` type for rho-core operations.
pub type Result<T> = std::result::Result<T, RhoError>;
