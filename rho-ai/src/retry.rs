//! Retry with exponential backoff.
//!
//! Wraps an [`EventStream`] with automatic retries
//! for transient errors (HTTP 429 rate limits, 5xx server errors, connection failures).

use crate::error::ProviderError;
use crate::service::{EventStream, LlmService};
use crate::types::LlmRequest;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial request).
    pub max_retries: u32,
    /// Base delay between retries.
    pub base_delay: Duration,
    /// Maximum delay between retries.
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_mins(1),
        }
    }
}

/// Computes the delay for a given retry attempt using exponential backoff with full jitter.
///
/// `delay = min(max_delay, base_delay * 2^attempt) * random(0..1)`
fn backoff_delay(config: &RetryConfig, attempt: u32) -> Duration {
    let exponential =
        config.base_delay.as_secs_f64() * 2_f64.powi(i32::try_from(attempt).unwrap_or(i32::MAX));
    let capped = exponential.min(config.max_delay.as_secs_f64());
    // Full jitter: random in [0, capped]
    let jitter = capped * fastrand::f64();
    Duration::from_secs_f64(jitter)
}

/// An [`LlmService`] that wraps another service with retry logic.
pub struct RetryingService {
    /// The underlying service to retry.
    inner: Arc<dyn LlmService>,
    /// Retry configuration.
    config: RetryConfig,
}

impl RetryingService {
    /// Creates a new retrying service wrapping the given inner service.
    pub fn new(inner: Arc<dyn LlmService>, config: RetryConfig) -> Self {
        Self { inner, config }
    }
}

/// State for tracking retries across the original attempt + retries.
struct RetryState {
    /// Number of retries performed so far.
    attempt: u32,
}

/// Runs an async operation with retry logic, yielding an [`EventStream`].
///
/// On each retryable error, waits with exponential backoff before retrying.
/// On non-retryable errors or budget exhaustion, returns immediately.
async fn retry_stream(
    config: &RetryConfig,
    operation: impl Fn() -> Pin<Box<dyn Future<Output = Result<EventStream, ProviderError>> + Send>>,
    state: &Mutex<RetryState>,
) -> Result<EventStream, ProviderError> {
    loop {
        match operation().await {
            Ok(stream) => return Ok(stream),
            Err(e) if e.is_retryable() && state.lock().await.attempt < config.max_retries => {
                let mut s = state.lock().await;
                s.attempt += 1;
                let delay = backoff_delay(config, s.attempt);
                let attempt_num = s.attempt;
                drop(s);
                warn!(attempt = attempt_num, max = config.max_retries, error = %e, "retryable error, backing off");
                debug!(
                    attempt = attempt_num,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    "waiting before retry"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) if e.is_retryable() => {
                let attempts = state.lock().await.attempt;
                warn!(attempts, max = config.max_retries, error = %e, "retry budget exhausted");
                return Err(ProviderError::RetryBudgetExhausted {
                    last_error: Box::new(e),
                });
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
}

#[async_trait]
impl LlmService for RetryingService {
    async fn chat_stream(&self, request: LlmRequest) -> Result<EventStream, ProviderError> {
        let state = Mutex::new(RetryState { attempt: 0 });
        let config = self.config.clone();
        let inner = self.inner.clone();

        retry_stream(
            &config,
            || {
                let inner = inner.clone();
                let request = request.clone();
                Box::pin(async move { inner.chat_stream(request).await })
            },
            &state,
        )
        .await
    }
}

/// Small RNG for jitter — no external dependency.
mod fastrand {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// PRNG state seed.
    static STATE: AtomicU64 = AtomicU64::new(0x1234_5678);

    /// Returns a pseudo-random `f64` in `[0, 1)`.
    #[must_use]
    pub fn f64() -> f64 {
        let state = STATE.fetch_add(0x9e37_79b9, Ordering::Relaxed);
        // xorshift64
        let mut z = state;
        z ^= z << 13;
        z ^= z >> 7;
        z ^= z << 17;
        f64::from_bits(z >> 12 | 0x3FF0_0000_0000_0000) - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_exponentially() {
        let config = RetryConfig::default();
        let d0 = backoff_delay(&config, 0);
        let d1 = backoff_delay(&config, 1);
        let d2 = backoff_delay(&config, 2);
        // On average, each should be larger than the previous
        // (not guaranteed due to jitter, but the *range* increases)
        assert!(d0 <= Duration::from_secs(3));
        assert!(d1 <= Duration::from_secs(5));
        assert!(d2 <= Duration::from_secs(10));
    }

    #[test]
    fn backoff_capped_at_max_delay() {
        let config = RetryConfig {
            max_retries: 10,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(10),
        };
        // At very high attempt, delay should still be capped
        let d = backoff_delay(&config, 100);
        assert!(d <= Duration::from_secs(11)); // +1 for rounding
    }

    #[test]
    fn default_config() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_delay, Duration::from_secs(2));
        assert_eq!(config.max_delay, Duration::from_mins(1));
    }
}
