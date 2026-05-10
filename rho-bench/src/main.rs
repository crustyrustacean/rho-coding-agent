//! rho-bench — benchmark harness for evaluating rho across local models.
//!
//! Runs rho's eval tasks against one or more local models and produces
//! structured comparison reports with timing, token usage, and pass/fail results.

mod comparison;
mod harness;
mod persistence;

use std::path::PathBuf;

use clap::Parser;
use rho_eval::tasks::all_tasks;

/// rho-bench — benchmark rho coding agent across local models.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Comma-separated list of model identifiers to benchmark.
    ///
    /// Each model is tested against all eval tasks. Models are loaded from
    /// the server at the configured endpoint (default: localhost:1234).
    ///
    /// Example: --models qwen3-6b,qwen3-32b,llama3-8b
    #[arg(short, long)]
    models: Option<String>,

    /// Task IDs to run (comma-separated). If omitted, all tasks are run.
    ///
    /// Example: --tasks `fix_e0308_type_mismatch,fix_unused_import`
    #[arg(short = 't', long)]
    tasks: Option<String>,

    /// Number of times to repeat each (model, task) pair.
    ///
    /// Reports the pass rate across repeats. Higher values give more
    /// reliable results for non-deterministic models.
    #[arg(short, long, default_value = "1")]
    repeats: u32,

    /// Model API endpoint URL.
    #[arg(long, default_value = "http://localhost:1234/v1/chat/completions")]
    endpoint: String,

    /// Output format: "table" (terminal table) or "json" (machine-readable).
    #[arg(short, long, default_value = "table")]
    output: String,

    /// Directory to write result files.
    #[arg(long, default_value = "bench-results")]
    results_dir: PathBuf,

    /// Use compact system prompt (for small-context models).
    #[arg(long)]
    compact: bool,

    /// Context window token budget.
    #[arg(long)]
    token_budget: Option<u32>,

    /// Maximum agent loop iterations per task.
    #[arg(long, default_value = "32")]
    max_iterations: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Tracing ---
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,rustls=warn,hyper=warn,reqwest=warn")
            }),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    // --- Resolve models ---
    let model_ids = resolve_models(&cli).await?;

    // --- Resolve tasks ---
    let all_tasks = all_tasks();
    let task_ids: Vec<String> = cli.tasks.as_deref().map_or_else(
        || all_tasks.iter().map(|t| t.id().to_string()).collect(),
        |s| s.split(',').map(str::trim).map(String::from).collect(),
    );
    let tasks: Vec<_> = all_tasks
        .into_iter()
        .filter(|t| task_ids.iter().any(|id| id == t.id()))
        .collect();

    if tasks.is_empty() {
        anyhow::bail!("no tasks matched. Available: {}", all_task_ids_csv());
    }

    eprintln!(
        "rho-bench: {} models × {} tasks × {} repeats = {} runs",
        model_ids.len(),
        tasks.len(),
        cli.repeats,
        model_ids.len() * tasks.len() * cli.repeats as usize,
    );
    eprintln!("endpoint: {}", cli.endpoint);
    eprintln!();

    // --- Run benchmarks ---
    let runs = harness::run_benchmarks(
        &model_ids,
        &tasks,
        cli.repeats,
        &cli.endpoint,
        cli.compact,
        cli.token_budget,
        cli.max_iterations,
    )
    .await;

    // --- Persist results ---
    persistence::save_results(&runs, &cli.results_dir)?;

    // --- Display ---
    match cli.output.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&runs)?;
            println!("{json}");
        }
        _ => {
            comparison::print_table(&runs);
        }
    }

    // Exit non-zero if any model scored below 100%
    let all_passed = runs.iter().all(|r| r.pass_count() == r.total());
    if !all_passed {
        std::process::exit(1);
    }

    Ok(())
}

/// Resolve model IDs from CLI flag or auto-detect from server.
async fn resolve_models(cli: &Cli) -> anyhow::Result<Vec<String>> {
    if let Some(ref models_str) = cli.models {
        return Ok(models_str
            .split(',')
            .map(str::trim)
            .map(String::from)
            .collect());
    }

    // Auto-detect: query the server for loaded models.
    eprintln!("no --models specified, querying server...");
    let client = rho_core::LocalChatClient::with_endpoint(&cli.endpoint);
    let list = client
        .list_models()
        .await
        .map_err(|e| anyhow::anyhow!("cannot query /v1/models at {}: {e}", cli.endpoint))?;
    if list.data.is_empty() {
        anyhow::bail!(
            "no models loaded on the server. Load a model and try again, or specify --models."
        );
    }
    let ids: Vec<String> = list.data.iter().map(|m| m.id.clone()).collect();
    eprintln!("auto-detected {} model(s): {}", ids.len(), ids.join(", "));
    Ok(ids)
}

/// Build a comma-separated string of all available task IDs.
fn all_task_ids_csv() -> String {
    rho_eval::tasks::all_tasks()
        .iter()
        .map(|t| t.id().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
