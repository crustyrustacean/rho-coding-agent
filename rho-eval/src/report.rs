//! Eval report types — run summary with prompt version tracking.

use crate::task::{TaskOutcome, TaskVerdict};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A single eval run: a collection of task outcomes with metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalRun {
    /// SHA-256 hash of the base prompt used for this run.
    pub prompt_base_sha256: String,
    /// SHA-256 hash of the full assembled system prompt.
    pub prompt_composition_sha256: String,
    /// Individual task outcomes.
    pub outcomes: Vec<TaskOutcome>,
}

impl EvalRun {
    /// Create a new eval run.
    pub fn new(prompt_base: &str, prompt_composition: &str) -> Self {
        Self {
            prompt_base_sha256: sha256_hex(prompt_base),
            prompt_composition_sha256: sha256_hex(prompt_composition),
            outcomes: Vec::new(),
        }
    }

    /// Add a task outcome.
    pub fn add_outcome(&mut self, outcome: TaskOutcome) {
        self.outcomes.push(outcome);
    }

    /// Number of tasks that passed.
    pub fn pass_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.verdict == TaskVerdict::Pass)
            .count()
    }

    /// Number of tasks that failed.
    pub fn fail_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.verdict == TaskVerdict::Fail)
            .count()
    }

    /// Total number of tasks.
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    /// Pass rate as a fraction (0.0 to 1.0).
    pub fn pass_rate(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.pass_count() as f64 / self.total() as f64
        }
    }
}

/// An eval report that can be compared against a previous run for regression
/// detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalReport {
    /// The eval run data.
    pub run: EvalRun,
}

impl EvalReport {
    /// Create a report from a run.
    pub fn new(run: EvalRun) -> Self {
        Self { run }
    }

    /// Check for regressions against a previous report.
    ///
    /// Returns `Some(message)` if the pass count dropped by more than
    /// `threshold` tasks compared to the previous report.
    pub fn check_regression(&self, previous: &Self, threshold: usize) -> Option<String> {
        let current_passes = self.run.pass_count();
        let previous_passes = previous.run.pass_count();

        if previous_passes > current_passes && (previous_passes - current_passes) > threshold {
            Some(format!(
                "regression detected: pass count dropped from {previous_passes} to {current_passes} (threshold: {threshold})"
            ))
        } else {
            None
        }
    }

    /// Serialize the report to TOML.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_else(|e| format!("# serialization error: {e}"))
    }
}

/// Compute the hex-encoded SHA-256 hash of a string.
fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskOutcome;

    fn sample_outcome(id: &str, verdict: TaskVerdict) -> TaskOutcome {
        TaskOutcome {
            task_id: id.to_owned(),
            task_name: format!("Task {id}"),
            verdict,
            explanation: String::new(),
        }
    }

    #[test]
    fn eval_run_counts_pass_and_fail() {
        let mut run = EvalRun::new("base", "composed");
        run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        run.add_outcome(sample_outcome("2", TaskVerdict::Fail));
        run.add_outcome(sample_outcome("3", TaskVerdict::Pass));

        assert_eq!(run.pass_count(), 2);
        assert_eq!(run.fail_count(), 1);
        assert_eq!(run.total(), 3);
    }

    #[test]
    fn eval_run_pass_rate() {
        let mut run = EvalRun::new("base", "composed");
        run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        run.add_outcome(sample_outcome("2", TaskVerdict::Fail));

        let rate = run.pass_rate();
        assert!((rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn eval_run_empty_pass_rate_is_zero() {
        let run = EvalRun::new("base", "composed");
        assert!((run.pass_rate()).abs() < f64::EPSILON);
    }

    #[test]
    fn eval_run_has_prompt_hashes() {
        let run = EvalRun::new("hello", "world");
        assert!(!run.prompt_base_sha256.is_empty());
        assert!(!run.prompt_composition_sha256.is_empty());
        assert_ne!(run.prompt_base_sha256, run.prompt_composition_sha256);
    }

    #[test]
    fn eval_run_round_trips_serde() {
        let mut run = EvalRun::new("base", "composed");
        run.add_outcome(sample_outcome("1", TaskVerdict::Pass));

        let json = serde_json::to_string(&run).unwrap();
        let back: EvalRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pass_count(), 1);
    }

    #[test]
    fn regression_detected_when_passes_drop() {
        let mut prev_run = EvalRun::new("base", "composed");
        prev_run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        prev_run.add_outcome(sample_outcome("2", TaskVerdict::Pass));
        prev_run.add_outcome(sample_outcome("3", TaskVerdict::Pass));
        let prev = EvalReport::new(prev_run);

        let mut curr_run = EvalRun::new("base", "composed");
        curr_run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        curr_run.add_outcome(sample_outcome("2", TaskVerdict::Fail));
        curr_run.add_outcome(sample_outcome("3", TaskVerdict::Fail));
        let curr = EvalReport::new(curr_run);

        // Threshold of 1: dropping by 2 should trigger.
        let regression = curr.check_regression(&prev, 1);
        assert!(regression.is_some());
        assert!(regression.unwrap().contains("dropped from 3 to 1"));
    }

    #[test]
    fn no_regression_within_threshold() {
        let mut prev_run = EvalRun::new("base", "composed");
        prev_run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        prev_run.add_outcome(sample_outcome("2", TaskVerdict::Pass));
        let prev = EvalReport::new(prev_run);

        let mut curr_run = EvalRun::new("base", "composed");
        curr_run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        curr_run.add_outcome(sample_outcome("2", TaskVerdict::Fail));
        let curr = EvalReport::new(curr_run);

        // Threshold of 2: dropping by 1 should not trigger.
        assert!(curr.check_regression(&prev, 2).is_none());
    }

    #[test]
    fn no_regression_when_passes_improve() {
        let mut prev_run = EvalRun::new("base", "composed");
        prev_run.add_outcome(sample_outcome("1", TaskVerdict::Fail));
        let prev = EvalReport::new(prev_run);

        let mut curr_run = EvalRun::new("base", "composed");
        curr_run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        let curr = EvalReport::new(curr_run);

        assert!(curr.check_regression(&prev, 0).is_none());
    }

    #[test]
    fn report_to_toml_produces_valid_output() {
        let mut run = EvalRun::new("base", "composed");
        run.add_outcome(sample_outcome("1", TaskVerdict::Pass));
        let report = EvalReport::new(run);

        let toml_str = report.to_toml();
        assert!(toml_str.contains("prompt_base_sha256"));
        assert!(toml_str.contains("task_id"));
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn sha256_hex_differs_for_different_input() {
        assert_ne!(sha256_hex("hello"), sha256_hex("world"));
    }
}
