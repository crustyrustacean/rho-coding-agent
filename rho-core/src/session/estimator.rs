//! Token estimation and calibration.
//!
//! The [`TokenEstimator`] trait abstracts token counting so the session tree can
//! make budget-aware decisions without coupling to any specific tokenizer. The
//! default [`HeuristicEstimator`] uses a per-model characters-per-token ratio
//! that self-corrects against API ground truth via exponential moving average.
//!
//! # Bootstrap ratios
//!
//! Known model families ship with pre-configured ratios:
//!
//! | Model family | Chars/token |
//! |---|---|
//! | Gemma | 3.5 |
//! | Qwen | 2.5 |
//! | Claude | 3.7 |
//! | GPT-4 | 4.0 |
//! | Llama | 3.5 |
//! | Unknown | 2.5 (conservative) |
//!
//! After the first real API response for a model, the calibrator updates the
//! ratio based on actual `prompt_tokens`. Convergence is typically within
//! 10% error by the third calibration call.
//!
//! # Calibration
//!
//! After each API round-trip, call [`calibrate`] with the estimated token count
//! and the actual `prompt_tokens` from the response. The estimator applies an
//! exponential moving average (α = 0.3) to the observed chars-per-token ratio,
//! giving fast convergence without whiplash on outliers.
//!
//! [`calibrate`]: TokenEstimator::calibrate

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── TokenEstimator trait ──────────────────────────────────────────────────────

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
    /// Estimate the token count for `content` using the given `model`'s
    /// calibrated or bootstrap chars-per-token ratio.
    fn estimate(&self, model: &str, content: &str) -> usize;

    /// Update internal state given an actual token count from the API.
    ///
    /// `estimated` is what this estimator predicted; `actual` is what the API
    /// reported in `prompt_tokens`. Implementations may use this to refine
    /// future estimates.
    fn calibrate(&mut self, model: &str, estimated: usize, actual: usize);
}

// ── HeuristicEstimator ────────────────────────────────────────────────────────

/// A token estimator that uses per-model characters-per-token ratios,
/// self-correcting against API ground truth.
///
/// # Bootstrap values
///
/// Known model families start with empirically-derived ratios. Unknown models
/// bootstrap conservatively at 2.5 chars/token (suitable for dense tokenizers
/// like Qwen).
///
/// # Calibration
///
/// After each API round-trip, call [`calibrate`] with the estimated and actual
/// token counts. The estimator updates the per-model ratio using an exponential
/// moving average with α = 0.3:
///
/// ```text
/// ratio = α * (chars_in_request / actual_tokens) + (1 - α) * ratio
/// ```
///
/// This converges quickly (within 3 calls to <10% error) while being robust
/// against outlier responses.
///
/// # Serialization
///
/// The estimator's per-model ratios can be serialized to JSON and restored,
/// enabling calibration persistence across sessions (Phase 2.6+).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeuristicEstimator {
    /// Per-model chars-per-token ratio. Bootstrapped from defaults; refined by [`calibrate`](HeuristicEstimator::calibrate)().
    #[serde(default = "default_ratios")]
    ratios: HashMap<String, f32>,
}

/// α for the exponential moving average during calibration.
/// 0.3 gives fast convergence without whiplash on outliers.
const EMA_ALPHA: f32 = 0.3;

/// Conservative default for unknown models.
/// Matches Qwen's dense tokenizer — err on the side of over-estimating tokens.
const UNKNOWN_CHARS_PER_TOKEN: f32 = 2.5;

/// Returns the default bootstrap ratios for known model families.
fn default_ratios() -> HashMap<String, f32> {
    let mut m = HashMap::new();
    // Model family prefixes → chars/token
    m.insert("gemma".to_owned(), 3.5);
    m.insert("qwen".to_owned(), 2.5);
    m.insert("claude".to_owned(), 3.7);
    m.insert("gpt-4".to_owned(), 4.0);
    m.insert("gpt-4o".to_owned(), 4.0);
    m.insert("llama".to_owned(), 3.5);
    m
}

impl Default for HeuristicEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicEstimator {
    /// Create a new estimator with default bootstrap ratios.
    pub fn new() -> Self {
        Self {
            ratios: default_ratios(),
        }
    }

    /// Estimate tokens using a specific model's ratio.
    ///
    /// Falls back to prefix-matching (e.g., "Qwen/Qwen2.5-Coder-14B-Instruct"
    /// matches "qwen"), then to the unknown default.
    pub fn estimate_for_model(&self, model: &str, content: &str) -> usize {
        let ratio = self.ratio_for(model);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let tokens = (content.len() as f32 / ratio) as usize;
        tokens.max(1) // at least 1 token for non-empty content
    }

    /// Look up the chars-per-token ratio for a model.
    ///
    /// Tries exact match first, then prefix match (case-insensitive),
    /// then the unknown default.
    pub fn ratio_for(&self, model: &str) -> f32 {
        // Exact match
        if let Some(&ratio) = self.ratios.get(model) {
            return ratio;
        }

        // Substring match (case-insensitive): "Qwen/Qwen2.5-Coder-14B-Instruct" contains "qwen",
        // "google/gemma-4-26b-a4b" contains "gemma".
        let model_lower = model.to_ascii_lowercase();
        for (prefix, &ratio) in &self.ratios {
            if model_lower.contains(&prefix.to_ascii_lowercase()) {
                return ratio;
            }
        }

        UNKNOWN_CHARS_PER_TOKEN
    }

    /// The current per-model ratios (for inspection / serialization).
    pub fn ratios(&self) -> &HashMap<String, f32> {
        &self.ratios
    }
}

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, model: &str, content: &str) -> usize {
        self.estimate_for_model(model, content)
    }

    fn calibrate(&mut self, model: &str, estimated: usize, actual: usize) {
        if actual == 0 {
            // Avoid division by zero; skip this calibration point.
            return;
        }

        let current_ratio = self.ratio_for(model);

        // The actual chars-per-token observed in this request.
        // We know `estimated = chars / current_ratio`, so `chars = estimated * current_ratio`.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let chars = estimated as f32 * current_ratio;
        #[allow(clippy::cast_precision_loss)]
        let observed_ratio = chars / actual as f32;

        // Exponential moving average
        let new_ratio = EMA_ALPHA * observed_ratio + (1.0 - EMA_ALPHA) * current_ratio;

        // Store under the exact model name the caller provided.
        // Prefix-matched models get their own entry after first calibration.
        self.ratios.insert(model.to_owned(), new_ratio);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    use super::*;

    #[test]
    fn heuristic_estimator_returns_nonzero_for_nonempty() {
        let est = HeuristicEstimator::new();
        let tokens = est.estimate("test-model", "hello world");
        assert!(
            tokens > 0,
            "should estimate > 0 tokens for non-empty string"
        );
    }

    #[test]
    fn heuristic_estimator_returns_one_for_empty() {
        let est = HeuristicEstimator::new();
        // max(1) ensures at least 1 token even for empty
        assert!(est.estimate("test-model", "") >= 1);
    }

    #[test]
    fn known_models_get_bootstrap_ratios() {
        let est = HeuristicEstimator::new();
        assert_eq!(est.ratio_for("gemma"), 3.5);
        assert_eq!(est.ratio_for("qwen"), 2.5);
        assert_eq!(est.ratio_for("claude"), 3.7);
        assert_eq!(est.ratio_for("gpt-4"), 4.0);
        assert_eq!(est.ratio_for("llama"), 3.5);
    }

    #[test]
    fn unknown_model_gets_conservative_ratio() {
        let est = HeuristicEstimator::new();
        assert_eq!(est.ratio_for("some-unknown-model"), UNKNOWN_CHARS_PER_TOKEN);
    }

    #[test]
    fn substring_match_finds_ratio() {
        let est = HeuristicEstimator::new();
        // "Qwen/Qwen2.5-Coder-14B-Instruct" should match via substring "qwen"
        assert_eq!(
            est.ratio_for("Qwen/Qwen2.5-Coder-14B-Instruct"),
            2.5,
            "substring match should find qwen ratio"
        );
        // "google/gemma-4-26b-a4b" should match via substring "gemma"
        assert_eq!(
            est.ratio_for("google/gemma-4-26b-a4b"),
            3.5,
            "substring match should find gemma ratio"
        );
    }

    #[test]
    fn estimate_for_model_uses_model_ratio() {
        let est = HeuristicEstimator::new();
        // 100 chars / 3.5 ratio ≈ 28 tokens for gemma
        // 100 chars / 2.5 ratio = 40 tokens for qwen
        let gemma_tokens = est.estimate_for_model("gemma", "a".repeat(100).as_str());
        let qwen_tokens = est.estimate_for_model("qwen", "a".repeat(100).as_str());
        assert!(
            qwen_tokens > gemma_tokens,
            "qwen should estimate more tokens than gemma for the same content (denser tokenizer)"
        );
    }

    #[test]
    fn calibrate_updates_ratio() {
        let mut est = HeuristicEstimator::new();
        let before = est.ratio_for("test-model");
        assert_eq!(before, UNKNOWN_CHARS_PER_TOKEN);

        // Simulate: we estimated 100 tokens for 250 chars, but actual was 50.
        // That means the real ratio is 250/50 = 5.0 chars/token.
        // estimated = 100, so chars = 100 * 2.5 = 250. observed = 250/50 = 5.0.
        // new = 0.3 * 5.0 + 0.7 * 2.5 = 1.5 + 1.75 = 3.25
        est.calibrate("test-model", 100, 50);

        let after = est.ratio_for("test-model");
        assert!(
            after > before,
            "ratio should increase after observing higher actual chars/token"
        );
        let expected = EMA_ALPHA * 5.0 + (1.0 - EMA_ALPHA) * UNKNOWN_CHARS_PER_TOKEN;
        assert!(
            (after - expected).abs() < 0.01,
            "ratio should be {expected}, got {after}"
        );
    }

    #[test]
    fn calibrate_converges_within_10_percent_by_third_call() {
        let mut est = HeuristicEstimator::new();

        // Simulate a model where the true ratio is 3.0 chars/token.
        // Unknown bootstrap is 2.5.
        let true_ratio = 3.0_f32;
        let content_chars = 3000_usize; // 3000 chars

        for i in 0..5 {
            let current_ratio = est.ratio_for("converge-model");
            let estimated = (content_chars as f32 / current_ratio) as usize;
            let actual = (content_chars as f32 / true_ratio) as usize;
            est.calibrate("converge-model", estimated, actual);

            let new_ratio = est.ratio_for("converge-model");
            let error = (new_ratio - true_ratio).abs() / true_ratio;

            if i >= 2 {
                assert!(
                    error < 0.10,
                    "after calibration {i}, error should be < 10%, got {:.1}% (ratio={new_ratio:.3}, true={true_ratio})",
                    error * 100.0
                );
            }
        }
    }

    #[test]
    fn calibrate_handles_zero_actual_gracefully() {
        let mut est = HeuristicEstimator::new();
        let before = est.ratio_for("zero-model");
        est.calibrate("zero-model", 100, 0);
        let after = est.ratio_for("zero-model");
        // Should not change when actual is 0 (avoid division by zero)
        assert_eq!(before, after, "ratio should not change when actual=0");
    }

    #[test]
    fn calibration_does_not_affect_other_models() {
        let mut est = HeuristicEstimator::new();
        let gemma_before = est.ratio_for("gemma");
        est.calibrate("test-model", 100, 50);
        let gemma_after = est.ratio_for("gemma");
        assert_eq!(
            gemma_before, gemma_after,
            "calibrating one model should not affect another"
        );
    }

    #[test]
    fn serialization_round_trip() {
        let mut est = HeuristicEstimator::new();
        est.calibrate("test-model", 100, 50);
        let before = est.ratios().clone();

        let json = serde_json::to_string(&est).unwrap();
        let back: HeuristicEstimator = serde_json::from_str(&json).unwrap();

        assert_eq!(back.ratios(), &before);
    }

    #[test]
    fn default_estimator_has_bootstrap_ratios() {
        let est = HeuristicEstimator::default();
        // The default should include the bootstrap ratios
        assert!(est.ratios().contains_key("gemma"));
        assert!(est.ratios().contains_key("qwen"));
    }

    #[test]
    fn substring_match_is_case_insensitive() {
        let est = HeuristicEstimator::new();
        assert_eq!(est.ratio_for("GEMMA-4"), 3.5);
        assert_eq!(est.ratio_for("Qwen/Qwen2.5"), 2.5);
    }

    #[test]
    fn exact_match_takes_priority_over_prefix() {
        let mut est = HeuristicEstimator::new();
        // Calibrate "gemma" to a different value
        est.calibrate("gemma", 100, 200);
        let custom_ratio = est.ratio_for("gemma");
        assert_ne!(custom_ratio, 3.5, "exact match should override bootstrap");
    }
}
