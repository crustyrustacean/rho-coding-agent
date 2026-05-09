//! Eval task trait and outcome types.

use serde::{Deserialize, Serialize};

/// The result of evaluating a single task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskVerdict {
    /// The agent produced the expected outcome.
    Pass,
    /// The agent did not produce the expected outcome.
    Fail,
    /// The task could not be evaluated (e.g. timeout, tool error).
    Error,
}

impl std::fmt::Display for TaskVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Fail => write!(f, "FAIL"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

/// Performance metrics captured during a single task run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskMetrics {
    /// Wall-clock time for the run in milliseconds.
    pub duration_ms: u64,
    /// Total prompt (input) tokens consumed.
    pub token_input: u32,
    /// Total completion (output) tokens generated.
    pub token_output: u32,
    /// Number of agent loop iterations (tool-call rounds).
    pub agent_iterations: u32,
    /// Why the model stopped generating (e.g. "stop", "tool_calls", "length").
    pub finish_reason: Option<String>,
}

/// The outcome of running a single eval task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskOutcome {
    /// The task's unique identifier.
    pub task_id: String,
    /// Human-readable task name.
    pub task_name: String,
    /// Pass, fail, or error.
    pub verdict: TaskVerdict,
    /// Explanation of the verdict (e.g. what was expected vs what was produced).
    pub explanation: String,
    /// Performance metrics for this run.
    #[serde(default)]
    pub metrics: TaskMetrics,
}

impl TaskOutcome {
    /// Create a new outcome with the given verdict and explanation.
    pub fn new(
        task_id: impl Into<String>,
        task_name: impl Into<String>,
        verdict: TaskVerdict,
        explanation: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            task_name: task_name.into(),
            verdict,
            explanation: explanation.into(),
            metrics: TaskMetrics::default(),
        }
    }
}

/// A single eval task that the agent must complete.
///
/// Each task defines:
/// - A unique ID and human-readable name
/// - Input source code with a known defect or requirement
/// - A verification function that checks whether the agent's output is correct
///
/// Tasks are defined statically (no network, no model API). The eval harness
/// is responsible for running the task through the agent and collecting the
/// outcome.
pub trait EvalTask: Send + Sync {
    /// Unique identifier for this task (e.g. `"fix_e0308_type_mismatch"`).
    fn id(&self) -> &str;

    /// Human-readable name (e.g. "Fix E0308 type mismatch").
    fn name(&self) -> &str;

    /// Description of what the agent should do.
    fn description(&self) -> &str;

    /// The initial source file(s) for the task.
    ///
    /// Each tuple is `(relative_path, content)`.
    fn initial_files(&self) -> Vec<(&str, &str)>;

    /// The user prompt to send to the agent.
    fn user_prompt(&self) -> &str;

    /// Verify the agent's result.
    ///
    /// `files` contains the final file contents after the agent has acted.
    /// Each tuple is `(relative_path, content)`.
    fn verify(&self, files: &[(&str, &str)]) -> TaskOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_verdict_display() {
        assert_eq!(TaskVerdict::Pass.to_string(), "PASS");
        assert_eq!(TaskVerdict::Fail.to_string(), "FAIL");
        assert_eq!(TaskVerdict::Error.to_string(), "ERROR");
    }

    #[test]
    fn task_verdict_round_trips_serde() {
        for verdict in [TaskVerdict::Pass, TaskVerdict::Fail, TaskVerdict::Error] {
            let json = serde_json::to_string(&verdict).unwrap();
            let back: TaskVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(verdict, back);
        }
    }

    #[test]
    fn task_outcome_round_trips_serde() {
        let outcome = TaskOutcome::new("test_task", "Test Task", TaskVerdict::Pass, "all good");
        let json = serde_json::to_string(&outcome).unwrap();
        let back: TaskOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(outcome.task_id, back.task_id);
        assert_eq!(outcome.verdict, back.verdict);
    }

    #[test]
    fn task_metrics_default_is_zero() {
        let m = TaskMetrics::default();
        assert_eq!(m.duration_ms, 0);
        assert_eq!(m.token_input, 0);
        assert_eq!(m.token_output, 0);
        assert_eq!(m.agent_iterations, 0);
        assert!(m.finish_reason.is_none());
    }

    #[test]
    fn task_outcome_builder() {
        let outcome = TaskOutcome::new("id", "Name", TaskVerdict::Fail, "reason");
        assert_eq!(outcome.task_id, "id");
        assert_eq!(outcome.task_name, "Name");
        assert_eq!(outcome.verdict, TaskVerdict::Fail);
        assert_eq!(outcome.explanation, "reason");
        // Metrics default
        assert_eq!(outcome.metrics.duration_ms, 0);
    }
}
