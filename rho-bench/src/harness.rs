//! Benchmark harness — runs one (model, task) pair through rho and collects metrics.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rho_core::{
    AgentConfig, ApprovalGate, AutoApprovePolicy, ConfigLoader, LoopParams, NopObserver, Provider,
    RhoConfig, SandboxRoot, Session, TokenBudget, ToolRegistry, ToolRisk,
    compose_full_system_prompt, provider_factory, run_loop,
};
use rho_eval::{EvalRun, EvalTask, TaskMetrics, TaskOutcome};
use rho_tools::register_all;

/// A gate that auto-approves all tool calls. Used for non-interactive benchmarking.
struct BenchApprovalGate;

#[async_trait]
impl ApprovalGate for BenchApprovalGate {
    async fn request_approval(
        &self,
        _call: &rho_core::message::ModelToolCall,
        _risk: ToolRisk,
    ) -> bool {
        true
    }
}

/// An [`LlmService`] wrapper that counts token usage across all requests.
struct CountingService {
    /// The underlying LLM service.
    inner: Box<dyn rho_ai::LlmService>,
    /// Cumulative prompt tokens across all requests.
    prompt_tokens: Arc<AtomicU32>,
    /// Cumulative completion tokens across all requests.
    completion_tokens: Arc<AtomicU32>,
    /// Total number of API requests made.
    request_count: Arc<AtomicU32>,
}

impl CountingService {
    /// Create a new counting service wrapping the given inner service.
    fn new(inner: Box<dyn rho_ai::LlmService>) -> Self {
        Self {
            inner,
            prompt_tokens: Arc::new(AtomicU32::new(0)),
            completion_tokens: Arc::new(AtomicU32::new(0)),
            request_count: Arc::new(AtomicU32::new(0)),
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
impl rho_ai::LlmService for CountingService {
    async fn chat_stream(
        &self,
        request: rho_ai::LlmRequest,
    ) -> std::result::Result<rho_ai::EventStream, rho_ai::ProviderError> {
        self.request_count.fetch_add(1, Ordering::Relaxed);
        let event_stream = self.inner.chat_stream(request).await?;

        // Wrap the event stream to count tokens from the Done event.
        let prompt_tokens = self.prompt_tokens.clone();
        let completion_tokens = self.completion_tokens.clone();
        let mapped = futures::StreamExt::map(event_stream, move |result| {
            if let Ok(rho_ai::StreamEvent::Done { usage, .. }) = &result {
                prompt_tokens.fetch_add(
                    u32::try_from(usage.input_tokens).unwrap_or(u32::MAX),
                    Ordering::Relaxed,
                );
                completion_tokens.fetch_add(
                    u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
                    Ordering::Relaxed,
                );
            }
            result
        });

        Ok(Box::pin(mapped))
    }
}

/// Run all (model × task × repeat) combinations and collect eval runs.
///
/// Returns one [`EvalRun`] per model, containing outcomes for all tasks.
#[allow(clippy::too_many_arguments)]
pub async fn run_benchmarks(
    model_ids: &[String],
    tasks: &[Box<dyn EvalTask>],
    repeats: u32,
    endpoint: &str,
    api_key_env: Option<&str>,
    compact: bool,
    token_budget: Option<u32>,
    max_iterations: Option<u32>,
    sandbox_root: &Path,
) -> Vec<EvalRun> {
    // Load full two-tier config (same as rho).
    let rho_config = ConfigLoader::load(sandbox_root).unwrap_or_else(|e| {
        eprintln!("Warning: {e} — using defaults");
        RhoConfig::default()
    });

    // Build prompt_base for EvalRun's prompt hash.
    let prompt_base = if compact {
        rho_core::compact_prompt()
    } else {
        rho_core::base_prompt()
    };

    let mut runs = Vec::with_capacity(model_ids.len());

    for model_id in model_ids {
        eprintln!("━━━ Model: {model_id} ━━━");
        let mut run = EvalRun::new(prompt_base, prompt_base).with_model(model_id);
        #[allow(deprecated)]
        let provider = provider_factory(&rho_config, Some(endpoint), api_key_env);

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
                    provider.as_ref(),
                    &rho_config,
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
    provider: &dyn Provider,
    rho_config: &RhoConfig,
    compact: bool,
    token_budget: Option<u32>,
    max_iterations_override: Option<u32>,
) -> Result<TaskOutcome> {
    // Create a temp directory for this task run.
    let project_dir =
        create_task_project(task).context("failed to create task project directory")?;

    let sandbox = SandboxRoot::new(&project_dir).context("failed to create sandbox root")?;

    // Set up tool registry with auto-approve policy for benchmarking.
    let mut registry = ToolRegistry::new();
    register_all(&mut registry, sandbox.clone(), rho_config);

    // Build system prompt using shared construction (no context file scanning
    // in bench — task dirs are ephemeral and don't have project context files).
    let system_prompt = compose_full_system_prompt(&sandbox, &[], rho_config, None, compact);

    // Build agent config from the shared config, with auto-approve override.
    let mut agent_config = AgentConfig::from_config(rho_config);
    agent_config.approval_policy = Box::new(AutoApprovePolicy);
    if let Some(max_iterations) = max_iterations_override {
        agent_config.max_iterations = max_iterations;
    }

    // Apply token budget override, respecting the configured completion reserve.
    // Scale the reserve proportionally for small budgets so prompt_budget > 0.
    let budget = token_budget.unwrap_or(rho_config.agent.token_budget) as usize;
    let reserve = rho_config.agent.completion_reserve as usize;
    let effective_reserve = reserve.min(budget / 4); // cap reserve at 25% of total

    // Build redactor from config (respects enabled toggle and custom patterns).
    let redactor = rho_core::Redactor::from_config(
        rho_config.redaction.enabled,
        &rho_config.redaction.custom_patterns,
    );

    // Create an in-memory session.
    let mut session = Session::in_memory(
        model_id,
        Some(&system_prompt),
        registry.tool_definitions(),
        &project_dir,
    )
    .with_token_budget(TokenBudget::with_reserve(budget, effective_reserve))
    .with_redactor(redactor);

    // Wrap the provider's service to count token usage.
    let counting_service = CountingService::new(provider.clone_boxed_service());

    let gate = BenchApprovalGate;
    let cancel = rho_core::CancellationToken::new();

    // Run the agent loop and measure time.
    let start = Instant::now();
    let params = LoopParams {
        client: &counting_service,
        registry: &registry,
        config: &agent_config,
        cancel,
        gate: &gate,
        observer: &NopObserver,
        compaction_client: None,
    };
    let result = run_loop(&mut session, task.user_prompt(), &params).await;
    let elapsed = start.elapsed();

    // Extract metrics from the counting service.
    let (token_input, token_output, request_count) = counting_service.snapshot();

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

    // Suppress unused-variable warning for request_count — it's kept for future use.
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
