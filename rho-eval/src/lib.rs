//! rho-eval — behavioural benchmark suite for the rho coding agent.
//!
//! Defines canonical coding tasks with known correct outcomes and provides
//! automated scoring. Run `rho-bench --tasks all` to execute the full suite.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`task`] | [`EvalTask`] trait, [`TaskOutcome`], [`TaskVerdict`] |
//! | [`report`] | [`EvalReport`], [`EvalRun`] — run summary with prompt hashes |
//! | [`tasks`] | Built-in scenario-based eval tasks |

pub mod report;
pub mod task;
pub mod tasks;

pub use report::{EvalReport, EvalRun};
pub use task::{EvalTask, TaskMetrics, TaskOutcome, TaskVerdict};
