//! Error types for the [`LlmService`](rho_ai::LlmService) trait and
//! [`RhoAiClient`](super::RhoAiClient) implementation.
//!
//! This module defines [`ClientError`] for HTTP/API related errors, with a
//! [`Retryable`] trait implementation to determine retry semantics.

use reqwest::Error as ReqwestError;
use serde_json::Error as JsonError;

/// Errors that can occur during API client operations.
///
/// These errors are specific to HTTP communication with model providers
/// and JSON serialization/deserialization.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// An HTTP request to the model API failed.
    #[error("HTTP request failed: {0}")]
    Http(#[from] ReqwestError),

    /// A response body could not be parsed as JSON.
    #[error("JSON parsing failed: {0}")]
    Json(#[from] JsonError),

    /// An endpoint URL could not be parsed.
    #[error("bad endpoint URL: {0}")]
    UrlParse(#[from] url::ParseError),

    /// The model API returned a non-2xx HTTP response.
    ///
    /// Unlike [`ClientError::Http`] (which wraps a `reqwest::Error` from
    /// connection-level failures), this variant carries the status code and
    /// body explicitly, allowing [`Retryable::is_retryable`]
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
    RetryBudgetExhausted(u32, Box<ClientError>),

    /// The model stream stalled: no data arrived within the configured timeout.
    ///
    /// This guards against hung connections where the model server (or an
    /// upstream proxy) accepts the request but never emits a first token, or
    /// stalls partway through a response. Without this, the agent loop would
    /// block indefinitely — in practice until the upstream's own idle timeout
    /// closes the connection, often surfacing downstream as a silent
    /// empty response.
    ///
    /// Always retryable: a stall is transient and the request can be resent
    /// after backoff.
    #[error("stream timed out waiting for {phase} after {elapsed_secs}s")]
    StreamTimeout {
        /// What the stream was waiting for when it timed out
        /// (e.g. `"first token"`, `"next chunk"`).
        phase: &'static str,
        /// How long we waited before giving up.
        elapsed_secs: u64,
    },

    /// A streaming response failed mid-stream (the connection was dropped or
    /// the body was malformed).
    ///
    /// This is the failure mode behind "empty reply" incidents: an upstream
    /// proxy severs the SSE connection at its own idle timeout (often ~120s),
    /// reqwest reports a body-decode error, and the partial response —
    /// frequently reasoning-only with no answer text — must be retried rather
    /// than surfaced as a truncated success.
    ///
    /// Always retryable: a mid-stream break is transient.
    #[error("stream error: {message}")]
    StreamError {
        /// Description of the stream failure.
        message: String,
    },
}

impl ClientError {
    /// Create an `HttpError` from a status code and response body.
    pub fn http_error(status: u16, message: impl Into<String>) -> Self {
        ClientError::HttpError {
            status,
            message: message.into(),
        }
    }

    /// Create a `RetryBudgetExhausted` error.
    pub fn retry_budget_exhausted(attempts: u32, last_error: ClientError) -> Self {
        ClientError::RetryBudgetExhausted(attempts, Box::new(last_error))
    }
}

/// Determines whether an error is retryable.
///
/// Retryable: HTTP 429, 500, 502, 503, 504, or network-level errors (no status).
/// Not retryable: JSON decode errors (indicate a schema mismatch, not a
/// transient failure), request builder errors (bad URL, etc.), and
/// permanent client errors (400, 401, 403, 404, etc.).
pub trait Retryable {
    /// Returns `true` if this error is transient and the operation may be retried.
    fn is_retryable(&self) -> bool;
}

impl Retryable for ClientError {
    fn is_retryable(&self) -> bool {
        match self {
            ClientError::Http(e) => {
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
            ClientError::HttpError { status, .. } => {
                matches!(status, 429 | 500 | 502 | 503 | 504)
            }
            // A stream stall is transient — the request can be resent after backoff.
            ClientError::StreamTimeout { .. } | ClientError::StreamError { .. } => true,
            ClientError::Json(_)
            | ClientError::RetryBudgetExhausted(_, _)
            | ClientError::UrlParse(_) => false,
        }
    }
}

/// A specialised `Result` type for client operations.
pub type ClientResult<T> = std::result::Result<T, ClientError>;

// ── Conversion from rho-ai ProviderError ──────────────────────────────────────

impl From<rho_ai::ProviderError> for ClientError {
    fn from(err: rho_ai::ProviderError) -> Self {
        match err {
            rho_ai::ProviderError::Http { source } => ClientError::Http(source),
            rho_ai::ProviderError::HttpStatus {
                status,
                body,
                retryable: _,
            } => {
                ClientError::HttpError {
                    status,
                    message: body.unwrap_or_default(),
                }
                // Note: retryable is preserved at the ProviderError level.
                // ClientError::Retryable checks the status code.
            }
            rho_ai::ProviderError::Sse { message } => ClientError::StreamError { message },
            rho_ai::ProviderError::Response { message, .. } => {
                ClientError::HttpError { status: 0, message }
            }
            rho_ai::ProviderError::RetryBudgetExhausted { last_error } => {
                let attempts = 0; // We don't have the count from rho-ai
                ClientError::retry_budget_exhausted(attempts, (*last_error).into())
            }
        }
    }
}

// Backward compatibility: convert ClientError to RhoError
impl From<ClientError> for crate::error::RhoError {
    fn from(error: ClientError) -> Self {
        crate::error::RhoError::Client(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_error_from_status() {
        let error = ClientError::http_error(500, "Internal Server Error".to_string());
        assert!(matches!(error, ClientError::HttpError { status: 500, .. }));
    }

    #[test]
    fn test_retry_budget_exhausted() {
        let http_error = ClientError::http_error(503, "Service Unavailable".to_string());
        let error = ClientError::retry_budget_exhausted(5, http_error);
        assert!(matches!(error, ClientError::RetryBudgetExhausted(5, _)));
    }

    #[test]
    fn test_retryable_server_errors() {
        assert!(ClientError::http_error(500, "Internal Server Error".to_string()).is_retryable());
        assert!(ClientError::http_error(502, "Bad Gateway".to_string()).is_retryable());
        assert!(ClientError::http_error(503, "Service Unavailable".to_string()).is_retryable());
        assert!(ClientError::http_error(504, "Gateway Timeout".to_string()).is_retryable());
    }

    #[test]
    fn test_retryable_rate_limit() {
        assert!(ClientError::http_error(429, "Too Many Requests".to_string()).is_retryable());
    }

    #[test]
    fn test_not_retryable_client_errors() {
        assert!(!ClientError::http_error(400, "Bad Request".to_string()).is_retryable());
        assert!(!ClientError::http_error(401, "Unauthorized".to_string()).is_retryable());
        assert!(!ClientError::http_error(403, "Forbidden".to_string()).is_retryable());
        assert!(!ClientError::http_error(404, "Not Found".to_string()).is_retryable());
    }

    #[test]
    fn test_not_retryable_json_errors() {
        let json_error = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let error = ClientError::Json(json_error);
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_not_retryable_retry_budget_exhausted() {
        let http_error = ClientError::http_error(503, "Service Unavailable".to_string());
        let error = ClientError::retry_budget_exhausted(5, http_error);
        assert!(!error.is_retryable());
    }

    #[test]
    fn test_stream_timeout_is_retryable() {
        let error = ClientError::StreamTimeout {
            phase: "first token",
            elapsed_secs: 90,
        };
        assert!(error.is_retryable());
        assert!(error.to_string().contains("first token"));
        assert!(error.to_string().contains("90s"));
    }

    #[test]
    fn test_stream_error_is_retryable() {
        let error = ClientError::StreamError {
            message: "SSE byte stream error: error decoding response body".to_owned(),
        };
        assert!(error.is_retryable());
        assert!(error.to_string().contains("error decoding response body"));
    }

    #[test]
    fn test_provider_sse_error_maps_to_retryable_stream_error() {
        // The actual incident: an upstream proxy severs the SSE connection,
        // reqwest reports a body-decode error, rho-ai surfaces it as
        // ProviderError::Sse. It must map to a retryable ClientError so the
        // agent loop retries instead of silently emitting a truncated reply.
        let provider_err = rho_ai::ProviderError::Sse {
            message: "error decoding response body".to_owned(),
        };
        let client_err: ClientError = provider_err.into();
        assert!(matches!(client_err, ClientError::StreamError { .. }));
        assert!(client_err.is_retryable());
    }

    #[test]
    fn test_client_error_display() {
        let error = ClientError::http_error(500, "Internal Server Error".to_string());
        assert_eq!(
            error.to_string(),
            "model API returned HTTP 500: Internal Server Error"
        );
    }

    #[test]
    fn test_retry_budget_exhausted_display() {
        let http_error = ClientError::http_error(503, "Service Unavailable".to_string());
        let error = ClientError::retry_budget_exhausted(3, http_error);
        let display = error.to_string();
        assert!(display.contains("retry budget exhausted after 3 attempts"));
        assert!(display.contains("503"));
    }
}
