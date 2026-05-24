//! Provider errors.
//!
//! Covers all failure modes across HTTP transport, SSE parsing,
//! and provider-specific issues.

use std::fmt;

/// Errors from LLM provider interactions.
#[derive(Debug)]
pub enum ProviderError {
    /// The HTTP request failed (network, DNS, TLS, etc.).
    Http {
        /// The underlying error from reqwest.
        source: reqwest::Error,
    },

    /// The HTTP request succeeded but returned a non-success status code.
    HttpStatus {
        /// The status code.
        status: u16,
        /// The response body, if available.
        body: Option<String>,
        /// Whether this error is retryable (429, 5xx).
        retryable: bool,
    },

    /// The SSE stream was malformed or ended unexpectedly.
    Sse {
        /// Description of the parse failure.
        message: String,
    },

    /// The provider returned valid JSON but it didn't match the expected schema.
    Response {
        /// Description of what went wrong.
        message: String,
        /// The raw JSON that couldn't be parsed (truncated if large).
        raw: Option<String>,
    },

    /// The retry budget was exhausted.
    RetryBudgetExhausted {
        /// The last error that triggered the retry.
        last_error: Box<ProviderError>,
    },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { source } => write!(f, "HTTP request failed: {source}"),
            Self::HttpStatus { status, body, .. } => {
                write!(f, "HTTP {status}")?;
                if let Some(b) = body {
                    let preview = b.chars().take(200).collect::<String>();
                    write!(f, ": {preview}")?;
                }
                Ok(())
            }
            Self::Sse { message } => write!(f, "SSE parse error: {message}"),
            Self::Response { message, raw } => {
                write!(f, "unexpected response: {message}")?;
                if let Some(r) = raw {
                    let preview = r.chars().take(200).collect::<String>();
                    write!(f, " ({preview})")?;
                }
                Ok(())
            }
            Self::RetryBudgetExhausted { last_error } => {
                write!(f, "retry budget exhausted, last error: {last_error}")
            }
        }
    }
}

impl std::error::Error for ProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http { source } => Some(source),
            Self::RetryBudgetExhausted { last_error } => Some(last_error),
            _ => None,
        }
    }
}

impl ProviderError {
    /// Returns `true` if the request should be retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::HttpStatus { retryable, .. } => *retryable,
            Self::Http { source } => {
                // Retry on connection errors, timeouts, etc.
                source.is_connect() || source.is_timeout() || source.is_request()
            }
            _ => false,
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(source: reqwest::Error) -> Self {
        Self::Http { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_error_display() {
        let err = ProviderError::Sse {
            message: "connection refused".to_string(),
        };
        assert!(err.to_string().contains("SSE parse error"));
    }

    #[test]
    fn http_status_display_with_body() {
        let err = ProviderError::HttpStatus {
            status: 429,
            body: Some("rate limited".to_string()),
            retryable: true,
        };
        assert_eq!(err.to_string(), "HTTP 429: rate limited");
    }

    #[test]
    fn http_status_retryable() {
        let retryable = ProviderError::HttpStatus {
            status: 429,
            body: None,
            retryable: true,
        };
        let not_retryable = ProviderError::HttpStatus {
            status: 400,
            body: None,
            retryable: false,
        };
        assert!(retryable.is_retryable());
        assert!(!not_retryable.is_retryable());
    }

    #[test]
    fn sse_error_display() {
        let err = ProviderError::Sse {
            message: "invalid event format".to_string(),
        };
        assert_eq!(err.to_string(), "SSE parse error: invalid event format");
    }

    #[test]
    fn response_error_display_truncates_long_raw() {
        let long_raw = "x".repeat(500);
        let err = ProviderError::Response {
            message: "missing field".to_string(),
            raw: Some(long_raw),
        };
        let display = err.to_string();
        assert!(display.contains("missing field"));
        // Raw is truncated to 200 chars
        assert!(display.len() < 400);
    }

    #[test]
    fn retry_budget_exhausted_display() {
        let inner = ProviderError::HttpStatus {
            status: 503,
            body: None,
            retryable: true,
        };
        let err = ProviderError::RetryBudgetExhausted {
            last_error: Box::new(inner),
        };
        assert!(err.to_string().contains("retry budget exhausted"));
    }

    use std::error::Error;

    #[test]
    fn retry_budget_exhausted_sources_inner() {
        let err = ProviderError::RetryBudgetExhausted {
            last_error: Box::new(ProviderError::Sse {
                message: "timeout".to_string(),
            }),
        };
        assert!(err.source().is_some());
    }
}
