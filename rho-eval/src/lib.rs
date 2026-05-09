//! rho-eval — behavioural benchmark suite for the rho coding agent.
//!
//! Defines canonical coding tasks with known correct outcomes and provides
//! automated scoring. Each eval task runs in an isolated in-memory session
//! and produces a pass/fail result.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`task`] | [`EvalTask`] trait, [`TaskOutcome`], [`TaskVerdict`] |
//! | [`report`] | [`EvalReport`], [`EvalRun`] — run summary with prompt hashes |
//! | [`tasks`] | Built-in eval task definitions |

pub mod report;
pub mod task;
pub mod tasks;

pub use report::{EvalReport, EvalRun};
pub use task::{EvalTask, TaskMetrics, TaskOutcome, TaskVerdict};
