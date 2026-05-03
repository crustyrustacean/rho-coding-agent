//! Token estimation and calibration.
//!
//! The [`TokenEstimator`] trait abstracts token counting so the session tree can
//! make budget-aware decisions without coupling to any specific tokenizer. The
//! default [`HeuristicEstimator`] uses a per-model characters-per-token ratio
//! that self-corrects against API ground truth.
//!
//! **This module is a stub** — the full implementation (bootstrap ratios, EMA
//! calibration, optional `tiktoken` integration) is Task 6. Only the trait
//! definition and a minimal default estimator are provided here so that
//! [`Session`](crate::session::Session) can hold a `Box<dyn TokenEstimator>`.

/// Estimates token counts for content strings.
///
/// Implementations range from simple heuristics (`chars / ratio`) to real
/// tokenizers (`tiktoken-rs`). The trait is `Send + Sync` so it can be shared
/// across async tasks.
///
/// # Calibration
///
/// After each API round-trip, the caller should invoke [`calibrate`] with the
/// estimated token count and the actual `prompt_tokens` from the response.
/// Implementations that support calibration (like [`HeuristicEstimator`]) will
/// update their per-model ratios; stateless implementations ignore the call.
///
/// [`calibrate`]: TokenEstimator::calibrate
pub trait TokenEstimator: Send + Sync {
    /// Estimate the token count for `content`.
    fn estimate(&self, content: &str) -> usize;

    /// Update internal state given an actual token count from the API.
    ///
    /// `estimated` is what this estimator predicted; `actual` is what the API
    /// reported in `prompt_tokens`. Implementations may use this to refine
    /// future estimates.
    fn calibrate(&mut self, model: &str, estimated: usize, actual: usize);
}

// ── HeuristicEstimator ────────────────────────────────────────────────────────

/// A token estimator that uses a fixed characters-per-token heuristic.
///
/// This is the default estimator used by [`Session`](crate::session::Session).
/// It uses a conservative ratio of 2.5 chars/token (suitable for models like
/// Qwen that tokenize densely) and does not currently support calibration.
///
/// Task 6 will replace this with a per-model calibrator that converges from
/// bootstrap ratios using exponential moving average.
#[derive(Debug, Default)]
pub struct HeuristicEstimator;

/// Conservative default: 2.5 chars per token.
/// Matches the "unknown model" bootstrap from the Phase 2.5 plan.
const DEFAULT_CHARS_PER_TOKEN: f32 = 2.5;

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, content: &str) -> usize {
        // Allow: heuristic token estimation is inherently approximate;
        // truncation and sign loss are acceptable trade-offs for speed.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let tokens = (content.len() as f32 / DEFAULT_CHARS_PER_TOKEN) as usize;
        tokens
    }

    fn calibrate(&mut self, _model: &str, _estimated: usize, _actual: usize) {
        // No-op: the full calibration logic is Task 6.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_estimator_returns_nonzero_for_nonempty() {
        let est = HeuristicEstimator;
        let tokens = est.estimate("hello world");
        assert!(
            tokens > 0,
            "should estimate > 0 tokens for non-empty string"
        );
    }

    #[test]
    fn heuristic_estimator_returns_zero_for_empty() {
        let est = HeuristicEstimator;
        assert_eq!(est.estimate(""), 0);
    }

    #[test]
    fn heuristic_calibrate_is_noop() {
        let mut est = HeuristicEstimator;
        let before = est.estimate("test");
        est.calibrate("any-model", before, 999);
        let after = est.estimate("test");
        assert_eq!(
            before, after,
            "HeuristicEstimator should not change after calibrate"
        );
    }
}
