//! Benchmark harness — runs one (model, task) pair through rho and collects metrics.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rho_core::{
    AgentConfig, ApprovalGate, AutoApprovePolicy, ChatClient, ChatRequest, LocalChatClient,
    ModelResponse, ModelToolCall, RhoConfig, SandboxRoot, Session, TokenBudget, ToolRegistry,
    ToolRisk, base_prompt, compact_prompt, run_loop,
};
use rho_eval::{EvalRun, EvalTask, TaskMetrics, TaskOutcome};
use rho_tools::register_all;

/// A gate that auto-approves all tool calls. Used for non-interactive benchmarking.
struct BenchApprovalGate;

#[async_trait]
impl ApprovalGate for BenchApprovalGate {
    async fn request_approval(&self, _call: &ModelToolCall, _risk: ToolRisk) -> bool {
        true
    }
}

/// A `ChatClient` wrapper that counts token usage across all requests.
struct CountingClient {
    /// The underlying model client.
    inner: LocalChatClient,
    /// Cumulative prompt tokens across all requests.
    prompt_tokens: AtomicU32,
    /// Cumulative completion tokens across all requests.
    completion_tokens: AtomicU32,
    /// Total number of API requests made.
    request_count: AtomicU32,
}

impl CountingClient {
    /// Create a new counting client wrapping the given inner client.
    fn new(inner: LocalChatClient) -> Self {
        Self {
            inner,
            prompt_tokens: AtomicU32::new(0),
            completion_tokens: AtomicU32::new(0),
            request_count: AtomicU32::new(0),
        }
    }

    /// Snapshot the current token and request counts.
    #[must_use]
    fn snapshot(&self) -> (u32, u32, u32) {
        (
            self.prompt_tokens.load(Ordering::Relaxed),
            self.completion_tokens.load(Ordering::Relaxed),
            self.request_count.load(Ordering::Relaxed),
        )
    }
}

#[async_trait]
impl ChatClient for CountingClient {
    async fn chat(&self, request: ChatRequest) -> rho_core::error::Result<ModelResponse> {
        let response = self.inner.chat(request).await?;
        self.prompt_tokens.fetch_add(
            u32::try_from(response.usage.prompt_tokens).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        self.completion_tokens.fetch_add(
            u32::try_from(response.usage.completion_tokens).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
        self.request_count.fetch_add(1, Ordering::Relaxed);
        Ok(response)
    }
}

/// Run all (model × task × repeat) combinations and collect eval runs.
///
/// Returns one [`EvalRun`] per model, containing outcomes for all tasks.
pub async fn run_benchmarks(
    model_ids: &[String],
    tasks: &[Box<dyn EvalTask>],
    repeats: u32,
    endpoint: &str,
    compact: bool,
    token_budget: Option<u32>,
    max_iterations: u32,
) -> Vec<EvalRun> {
    let prompt_base = if compact {
        compact_prompt()
    } else {
        base_prompt()
    };

    let mut runs = Vec::with_capacity(model_ids.len());

    for model_id in model_ids {
        eprintln!("━━━ Model: {model_id} ━━━");
        let mut run = EvalRun::new(prompt_base, prompt_base).with_model(model_id);
        let client = LocalChatClient::with_endpoint(endpoint);

        for task in tasks {
            for repeat in 1..=repeats {
                let label = if repeats > 1 {
                    format!("{} (repeat {repeat}/{repeats})", task.id())
                } else {
                    task.id().to_string()
                };

                eprintln!("  → {label}...");

                match run_single_task(
                    task.as_ref(),
                    model_id,
                    &client,
                    endpoint,
                    compact,
                    token_budget,
                    max_iterations,
                )
                .await
                {
                    Ok(outcome) => {
                        eprintln!(
                            "    {} {} [{}ms, {}+{} tokens, {} iters]",
                            match outcome.verdict {
                                rho_eval::TaskVerdict::Pass => "✅",
                                rho_eval::TaskVerdict::Fail => "❌",
                                rho_eval::TaskVerdict::Error => "⚠️ ",
                            },
                            outcome.task_name,
                            outcome.metrics.duration_ms,
                            outcome.metrics.token_input,
                            outcome.metrics.token_output,
                            outcome.metrics.agent_iterations,
                        );
                        run.outcomes.push(outcome);
                    }
                    Err(e) => {
                        eprintln!("    ⚠️  ERROR: {e}");
                        run.outcomes.push(TaskOutcome::new(
                            task.id(),
                            task.name(),
                            rho_eval::TaskVerdict::Error,
                            format!("harness error: {e}"),
                        ));
                    }
                }
            }
        }

        eprintln!("  Result: {}/{} passed", run.pass_count(), run.total());
        eprintln!();
        runs.push(run);
    }

    runs
}

/// Run a single task against a single model.
///
/// Creates a temp directory with the task's initial files, spins up an
/// in-memory Session, and runs the agent loop. After the agent completes,
/// reads back the files from disk and calls the task's verify function.
async fn run_single_task(
    task: &dyn EvalTask,
    model_id: &str,
    client: &LocalChatClient,
    endpoint: &str,
    compact: bool,
    token_budget: Option<u32>,
    max_iterations: u32,
) -> Result<TaskOutcome> {
    // Create a temp directory for this task run.
    let project_dir =
        create_task_project(task).context("failed to create task project directory")?;

    let sandbox = SandboxRoot::new(&project_dir).context("failed to create sandbox root")?;

    // Load a minimal rho config with all tools set to auto.
    let rho_config = bench_config(max_iterations, token_budget);

    // Set up tool registry.
    let mut registry = ToolRegistry::new();
    register_all(&mut registry, sandbox.clone(), &rho_config);

    // Build system prompt.
    let prompt_base: String = if compact {
        compact_prompt().to_owned()
    } else {
        base_prompt().to_owned()
    };
    let mut system_prompt = prompt_base;
    let root = sandbox.path().display();
    #[allow(clippy::format_push_string)]
    system_prompt.push_str(&format!(
        "\n\n# Environment\n\n\
         - Working directory (project root): `{root}`\n\
         - All relative file paths and shell commands resolve from this directory.\n\
         - Each `run_command` invocation starts a fresh process in this directory.\n\
           `cd` and `Set-Location` do not persist between commands — include the\n\
           full relative path from the project root in every command."
    ));
    system_prompt.push_str(
        "\n\n# Rust Tooling\n\n\
         - You have access to structured Rust compiler diagnostics via `cargo_check` and `cargo_clippy`.\n\
         - When code fails to compile, use `cargo_check` before attempting manual fixes.\n\
         - Trust machine-applicable suggestions from the compiler — apply them with `cargo_fix` or by\n\
           using the suggested replacement text in `edit_file`.\n\
         - Use `cargo_clippy` for code-quality lints beyond compilation errors.\n\
         - Use `rustc_explain` to look up detailed explanations for error codes (e.g. E0308).\n\
         - Use `cargo_test` to verify fixes — run the relevant tests after each change.\n\
         - Prefer the structured diagnostic tools over `run_command` with raw `cargo check` —\n\
           the tools parse JSON output and surface only actionable workspace diagnostics.",
    );

    // Create an in-memory session.
    let budget = token_budget.unwrap_or(rho_config.agent.token_budget) as usize;
    let mut session = Session::in_memory(
        model_id,
        Some(&system_prompt),
        registry.tool_schemas(),
        &project_dir,
    )
    .with_token_budget(TokenBudget::new(budget))
    .with_redactor(rho_core::Redactor::new());

    // Wrap the client to count token usage.
    let counting_client = CountingClient::new((*client).clone());

    // Build agent config with auto-approve policy.
    let agent_config = AgentConfig {
        max_iterations,
        retry_budget: 2,
        initial_backoff_ms: 250,
        approval_policy: Box::new(AutoApprovePolicy),
        ..AgentConfig::default()
    };

    let gate = BenchApprovalGate;
    let cancel = rho_core::CancellationToken::new();

    // Run the agent loop and measure time.
    let start = Instant::now();
    let result = run_loop(
        &mut session,
        task.user_prompt(),
        &counting_client,
        &registry,
        &agent_config,
        cancel,
        &gate,
    )
    .await;
    let elapsed = start.elapsed();

    // Extract metrics from the counting client.
    let (token_input, token_output, request_count) = counting_client.snapshot();

    // Count agent iterations by counting assistant messages in the session.
    let agent_iterations = count_assistant_entries(&session);

    let metrics = TaskMetrics {
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        token_input,
        token_output,
        agent_iterations,
        finish_reason: None,
    };

    // Read back the files from disk and verify.
    let final_files_owned =
        read_project_files(&project_dir, task.initial_files().iter().map(|(p, _)| *p));
    let final_refs: Vec<(&str, &str)> = final_files_owned
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mut outcome = task.verify(&final_refs);
    outcome.metrics = metrics;

    // Suppress unused-variable warning for endpoint — it's kept for future use.
    let _ = endpoint;
    let _ = request_count;

    // If the run_loop itself failed, override with Error.
    if let Err(e) = result {
        outcome.verdict = rho_eval::TaskVerdict::Error;
        outcome.explanation = format!("agent loop error: {e}");
    }

    // Clean up temp directory.
    let _ = std::fs::remove_dir_all(&project_dir);

    Ok(outcome)
}

/// Count the number of assistant entries in the session (proxy for agent iterations).
fn count_assistant_entries(session: &Session) -> u32 {
    use rho_core::ChatMessage;
    let mut count = 0u32;
    for entry in session.path_to_root() {
        if let rho_core::EntryPayload::Message(ChatMessage::Assistant { .. }) = &entry.payload {
            count = count.saturating_add(1);
        }
    }
    count
}

/// Create a temporary Cargo project directory with the task's initial files.
fn create_task_project(task: &dyn EvalTask) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("rho-bench-{}", task.id()));

    // Remove any previous run.
    let _ = std::fs::remove_dir_all(&dir);

    // Initialize a cargo lib project.
    std::process::Command::new("cargo")
        .args(["init", "--lib", &dir.to_string_lossy()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("cargo init failed")?;

    // Write the task's initial files.
    for (rel_path, content) in task.initial_files() {
        let full_path = dir.join(rel_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create directory {}", parent.display()))?;
        }
        std::fs::write(&full_path, content)
            .with_context(|| format!("cannot write {}", full_path.display()))?;
    }

    Ok(dir)
}

/// Build a minimal [`RhoConfig`] suitable for benchmarking.
///
/// All tools are set to auto-approve.
fn bench_config(max_iterations: u32, token_budget: Option<u32>) -> RhoConfig {
    use rho_core::config::{AgentLoopConfig, ApprovalAction, ApprovalConfig};

    let agent = if let Some(budget) = token_budget {
        AgentLoopConfig {
            max_iterations,
            token_budget: budget,
            ..AgentLoopConfig::default()
        }
    } else {
        AgentLoopConfig {
            max_iterations,
            ..AgentLoopConfig::default()
        }
    };

    let mut per_tool = std::collections::HashMap::new();
    for name in [
        "cargo_check",
        "cargo_clippy",
        "cargo_fix",
        "cargo_test",
        "edit_file",
        "write_file",
        "run_command",
        "read_file",
        "list_dir",
    ] {
        per_tool.insert(name.to_string(), ApprovalAction::Auto);
    }

    RhoConfig {
        agent,
        approval: ApprovalConfig { per_tool },
        ..RhoConfig::default()
    }
}

/// Read back the final file contents from the project directory.
///
/// Only reads files that were part of the task's initial file set.
fn read_project_files<'a>(
    project_dir: &Path,
    rel_paths: impl Iterator<Item = &'a str>,
) -> Vec<(String, String)> {
    rel_paths
        .filter_map(|rel| {
            let full = project_dir.join(rel);
            std::fs::read_to_string(&full)
                .ok()
                .map(|content| (rel.to_string(), content))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_config_sets_auto_approve() {
        let config = bench_config(16, Some(8192));
        assert_eq!(config.agent.max_iterations, 16);
        assert_eq!(config.agent.token_budget, 8192);
        assert_eq!(
            config.approval.per_tool.get("edit_file"),
            Some(&rho_core::config::ApprovalAction::Auto)
        );
    }
}
