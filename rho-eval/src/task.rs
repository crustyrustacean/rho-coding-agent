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
        let outcome = TaskOutcome {
            task_id: "test_task".to_owned(),
            task_name: "Test Task".to_owned(),
            verdict: TaskVerdict::Pass,
            explanation: "all good".to_owned(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let back: TaskOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(outcome.task_id, back.task_id);
        assert_eq!(outcome.verdict, back.verdict);
    }
}
