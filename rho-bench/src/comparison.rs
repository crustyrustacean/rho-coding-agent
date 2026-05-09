//! Multi-model comparison and terminal table display.

use std::fmt::Write as _;

use rho_eval::{EvalRun, TaskVerdict};

/// Print a formatted comparison table for all runs.
pub fn print_table(runs: &[EvalRun]) {
    if runs.is_empty() {
        println!("No benchmark results.");
        return;
    }

    // ── Header ────────────────────────────────────────────────────────────────
    println!("╭{:─<77}╮", "");
    println!("│{:^77}│", "rho-bench results");
    println!("╰{:─<77}╯", "");
    println!();

    // ── Summary table ─────────────────────────────────────────────────────────
    // Column widths: model=28, pass=8, fail=8, err=6, time=8, tokens=10
    println!(
        "{:<28} {:>6} {:>6} {:>6} {:>8} {:>10}",
        "Model", "Pass", "Fail", "Err", "Time", "Tokens"
    );
    println!(
        "{:-<28} {:->6} {:->6} {:->6} {:->8} {:->10}",
        "", "", "", "", "", ""
    );

    for run in runs {
        let passed = run.pass_count();
        let failed = run.fail_count();
        let errored = run.total() - passed - failed;
        let total_ms = run.total_duration_ms();
        let total_tokens = run.total_token_input() + run.total_token_output();

        let time_str = format_duration(total_ms);
        let token_str = format_tokens(total_tokens);
        let label = truncate_model(&run.model_id, 28);
        let total = run.total();

        println!(
            "{:<28} {passed:>2}/{total:<4} {failed:>2}/{total:<4} {errored:>2}/{total:<4} {:>8} {:>10}",
            label, time_str, token_str,
        );
    }

    println!();

    // ── Per-task breakdown ────────────────────────────────────────────────────
    if runs.len() > 1 {
        print_task_breakdown(runs);
    } else {
        print_single_run_tasks(&runs[0]);
    }
}

/// Print a per-task breakdown across multiple models.
fn print_task_breakdown(runs: &[EvalRun]) {
    // Collect all unique task IDs (preserving order from first run).
    let task_ids: Vec<String> = runs
        .first()
        .map(|r| r.outcomes.iter().map(|o| o.task_id.clone()).collect())
        .unwrap_or_default();

    // Column widths: task=30, each model=16
    let model_width = 16usize;

    // Header row
    let mut header = format!("{:<30}", "Task");
    for run in runs {
        let label = truncate_model(&run.model_id, model_width - 1);
        write!(header, " {:>model_width$}", label).unwrap();
    }
    println!("{header}");
    println!(
        "{:-<30}{}",
        "",
        "─".repeat(model_width * runs.len())
    );

    for task_id in &task_ids {
        let mut row = format!("{:<30}", truncate_task(task_id, 30));
        for run in runs {
            let outcome = run.outcomes.iter().find(|o| &o.task_id == task_id);
            let cell = match outcome {
                Some(o) => match o.verdict {
                    TaskVerdict::Pass => format!(
                        "✅ {:>5}ms {:>4}it",
                        o.metrics.duration_ms, o.metrics.agent_iterations
                    ),
                    TaskVerdict::Fail => format!(
                        "❌ {:>5}ms {:>4}it",
                        o.metrics.duration_ms, o.metrics.agent_iterations
                    ),
                    TaskVerdict::Error => "⚠️  error ".to_string(),
                },
                None => "  —      ".to_string(),
            };
            write!(row, " {:>model_width$}", cell).unwrap();
        }
        println!("{row}");
    }
}

/// Print per-task details for a single run.
fn print_single_run_tasks(run: &EvalRun) {
    println!(
        "{:<30} {:>5} {:>5} {:>5} {:>10} {:>10}",
        "Task", "Verdict", "Time", "Iters", "In Tokens", "Out Tokens"
    );
    println!(
        "{:-<30} {:-<7} {:-<7} {:-<7} {:-<12} {:-<12}",
        "", "", "", "", "", ""
    );

    for outcome in &run.outcomes {
        let verdict = match outcome.verdict {
            TaskVerdict::Pass => "✅",
            TaskVerdict::Fail => "❌",
            TaskVerdict::Error => "⚠️ ",
        };
        println!(
            "{:<30} {:>5} {:>4}ms {:>4}i {:>8} {:>8}",
            truncate_task(&outcome.task_id, 30),
            verdict,
            outcome.metrics.duration_ms,
            outcome.metrics.agent_iterations,
            outcome.metrics.token_input,
            outcome.metrics.token_output,
        );
    }
}

/// Format a duration in milliseconds as a human-readable string.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        let secs = ms as f64 / 1000.0;
        format!("{secs:.1}s")
    }
}

/// Format a token count as a human-readable string.
fn format_tokens(tokens: u32) -> String {
    if tokens < 1000 {
        format!("{tokens}")
    } else {
        format!("{:.1}k", tokens as f64 / 1000.0)
    }
}

/// Truncate a model ID to fit within `max_len` characters.
fn truncate_model(model_id: &str, max_len: usize) -> String {
    if model_id.len() <= max_len {
        model_id.to_string()
    } else {
        // Keep the last (max_len - 2) chars for readability.
        format!("..{}", &model_id[model_id.len() - (max_len - 2)..])
    }
}

/// Truncate a task ID to fit within `max_len` characters.
fn truncate_task(task_id: &str, max_len: usize) -> String {
    if task_id.len() <= max_len {
        task_id.to_string()
    } else {
        format!("{}..", &task_id[..max_len - 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_under_one_second() {
        assert_eq!(format_duration(42), "42ms");
    }

    #[test]
    fn format_duration_over_one_second() {
        assert_eq!(format_duration(5500), "5.5s");
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(500), "500");
    }

    #[test]
    fn format_tokens_large() {
        assert_eq!(format_tokens(18500), "18.5k");
    }

    #[test]
    fn truncate_model_short() {
        assert_eq!(truncate_model("qwen3-8b", 28), "qwen3-8b");
    }

    #[test]
    fn truncate_model_long() {
        let long = "very-long-model-name-that-exceeds-limit";
        assert_eq!(truncate_model(long, 20), "..that-exceeds-limit");
    }
}
