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

    /// The model API returned a non-2xx HTTP response.
    ///
    /// Unlike [`RhoError::Http`] (which wraps a `reqwest::Error` from
    /// connection-level failures), this variant carries the status code and
    /// body explicitly, allowing [`is_retryable()`](RhoError::is_retryable)
    /// to distinguish retryable server errors (500, 502, 503, 504) from
    /// permanent client errors (400, 401, 403, 404).
    #[error("model API returned HTTP {status}: {message}")]
    HttpError {
        /// The HTTP status code.
        status: u16,
        /// The response body (may be truncated).
        message: String,
    },

    /// Transient errors were retried until the retry budget was exhausted.
    ///
    /// The second element is the last error that triggered a retry, included
    /// so callers can diagnose what kept failing (e.g. HTTP 500 from the
    /// model server).
    #[error("retry budget exhausted after {0} attempts: {1}")]
    RetryBudgetExhausted(u32, Box<RhoError>),

    /// The agent loop was cancelled by the user or a cancellation token.
    #[error("cancelled")]
    Cancelled,

    /// A network request was blocked by the egress policy.
    #[error("egress blocked: {host}")]
    EgressBlocked {
        /// The hostname that was blocked.
        host: String,
    },

    /// The model API returned a response that violates the expected protocol.
    ///
    /// For example, the model returned an empty `tool_calls` array or a
    /// streaming outcome that is not yet supported.
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),

    /// An unexpected error occurred.
    #[error(transparent)]
    Unexpected(#[from] anyhow::Error),
}

impl RhoError {
    /// Returns `true` if this error is transient and the operation may be retried.
    ///
    /// Retryable: HTTP 429, 500, 502, 503, 504, or network-level errors (no status).
    /// Not retryable: JSON decode errors (indicate a schema mismatch, not a
    /// transient failure), request builder errors (bad URL, etc.), and
    /// permanent client errors (400, 401, 403, 404, etc.).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            RhoError::Http(e) => {
                // Decode errors (e.g. model returned unexpected JSON shape)
                // are not transient — retrying won't fix a schema mismatch.
                if e.is_decode() {
                    return false;
                }
                // Builder errors (bad URL, invalid header) are not transient.
                if e.is_builder() {
                    return false;
                }
                e.status()
                    .is_none_or(|s| matches!(s.as_u16(), 429 | 500 | 502 | 503 | 504))
            }
            RhoError::HttpError { status, .. } => {
                matches!(status, 429 | 500 | 502 | 503 | 504)
            }
            _ => false,
        }
    }
}

/// A specialised `Result` type for rho-core operations.
pub type Result<T> = std::result::Result<T, RhoError>;
