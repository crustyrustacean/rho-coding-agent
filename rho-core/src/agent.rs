//! Agent loop state machine.
//!
//! [`run_loop`] drives the conversation until the model stops or a budget is
//! exhausted. [`AgentState`] is the observable state for UIs and tests.
//! [`TransitionError`] classifies errors as retryable or fatal.

use crate::approval::{ApprovalGate, ApprovalPolicy, DefaultApprovalPolicy};
use crate::client::ChatClient;
use crate::conversation::AssistantResponse;
use crate::error::{Result, RhoError};
use crate::newtypes::ToolCallId;
use crate::session::Session;
use crate::tool::{CancellationToken, Tool, ToolRegistry, ToolResult};
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

// ── State and error types ─────────────────────────────────────────────────────

/// The observable state of the agent loop.
///
/// The loop transitions between these four states. Errors and retries are
/// *transition outcomes* — represented as `Result` and a separate retry counter
/// — not additional states.
///
/// Phase 4 will expose this via a state-change channel so the TUI can render
/// live status (spinner, approval prompt, tool progress bar, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Waiting for the next user message.
    Idle,
    /// Waiting for a model response.
    Thinking,
    /// Waiting for user approval before executing a tool.
    AwaitingApproval,
    /// Executing a tool.
    ExecutingTool,
}

/// Why a loop transition failed.
///
/// Retryable errors trigger exponential backoff; fatal errors terminate
/// immediately.
#[derive(Debug)]
pub enum TransitionError {
    /// Transient failure — the operation may be retried.
    Retryable(RhoError),
    /// Permanent failure — the loop must stop.
    Fatal(RhoError),
}

impl TransitionError {
    /// Wrap a [`RhoError`] in the appropriate variant.
    pub fn from_error(e: RhoError) -> Self {
        if e.is_retryable() {
            Self::Retryable(e)
        } else {
            Self::Fatal(e)
        }
    }

    /// Convert back into a [`RhoError`].
    pub fn into_error(self) -> RhoError {
        match self {
            Self::Retryable(e) | Self::Fatal(e) => e,
        }
    }
}

// ── AgentConfig ───────────────────────────────────────────────────────────────

/// Configuration for the agent loop.
pub struct AgentConfig {
    /// Maximum *retry attempts* on transient errors (not total attempts;
    /// the initial call is not counted). After this many retries, the loop
    /// fails with [`RhoError::RetryBudgetExhausted`].
    pub max_iterations: u32,
    /// Maximum *retry attempts* on transient errors before giving up. This is
    /// the number of retries, not the total number of attempts (initial + retries
    /// = 1 + `retry_budget`).
    pub retry_budget: u32,
    /// Base backoff in milliseconds. Each retry waits
    /// `initial_backoff_ms * 2^retry_number`, capped at 64× the base. So the
    /// first retry waits 2× the base, the second 4×, the third 8×, etc.
    pub initial_backoff_ms: u64,
    /// Policy that decides whether a tool call needs human approval.
    pub approval_policy: Box<dyn ApprovalPolicy>,
    /// Number of times a tool call with identical (name, arguments, output)
    /// may repeat before the agent injects a stuck-loop nudge into the
    /// conversation. Set to 0 to disable stuck-loop detection.
    pub stuck_loop_threshold: u32,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("max_iterations", &self.max_iterations)
            .field("retry_budget", &self.retry_budget)
            .field("initial_backoff_ms", &self.initial_backoff_ms)
            .field("approval_policy", &"<dyn ApprovalPolicy>")
            .field("stuck_loop_threshold", &self.stuck_loop_threshold)
            .finish()
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 32,
            retry_budget: 4,
            initial_backoff_ms: 500,
            approval_policy: Box::new(DefaultApprovalPolicy),
            stuck_loop_threshold: 3,
        }
    }
}

impl AgentConfig {
    /// Create an `AgentConfig` from a [`RhoConfig`], using the config-driven
    /// approval policy.
    ///
    /// Uses [`ConfigApprovalPolicy`] for per-tool approval overrides, falling
    /// back to [`DefaultApprovalPolicy`] for tools not listed in config.
    ///
    /// [`RhoConfig`]: crate::config::RhoConfig
    /// [`ConfigApprovalPolicy`]: crate::approval::ConfigApprovalPolicy
    pub fn from_config(config: &crate::config::RhoConfig) -> Self {
        use crate::approval::ConfigApprovalPolicy;
        Self {
            max_iterations: config.agent.max_iterations,
            retry_budget: config.agent.retry_budget,
            initial_backoff_ms: config.agent.initial_backoff_ms,
            approval_policy: Box::new(ConfigApprovalPolicy::new(config)),
            stuck_loop_threshold: config.agent.stuck_loop_threshold,
        }
    }
}

// ── run_loop ──────────────────────────────────────────────────────────────────

/// Run the agent loop until the model produces a final text reply.
///
/// Appends `message` as a user turn, then cycles through [`AgentState`]s:
///
/// `Idle` → `Thinking` → `AwaitingApproval`? → `ExecutingTool` → `Thinking` → …
///
/// Returns `Idle` (done) when the model issues a `Stop` response.
///
/// # State transitions
///
/// - `Thinking`: send conversation to model, with retry on transient errors.
/// - `AwaitingApproval`: consult `config.approval_policy`; if required, ask `gate`.
///   On denial, feed a synthetic denial result back and return to `Thinking`.
/// - `ExecutingTool`: run the tool, append the result. When multiple tool calls
///   are present, each is executed sequentially before returning to `Thinking`.
/// - `Idle`: model replied with text — done.
///
/// All tool calls in a single model response are executed sequentially; each result
/// is appended before re-sending to the model. Parallel execution is a future
/// optimisation.
///
/// # Errors
///
/// - [`RhoError::MaxIterationsExceeded`] — loop ran past `config.max_iterations`
/// - [`RhoError::RetryBudgetExhausted`] — transient error retried too many times
/// - Any fatal error from the client or tool registry
///
/// # State tracking
///
/// `state` transitions are assigned on every path through the loop. The
/// assignments are intentional stubs: Phase 4 will attach a state-change
/// channel here so the TUI can render live status. The lints are suppressed
/// until that wiring lands — do not remove the assignments.
///
/// # Instrumentation
///
/// The `run_loop` function carries a span with `input_len`. Each loop
/// iteration creates a child span (`agent_iteration`) with a single
/// `iteration` field. This per-iteration span is required because
/// `tracing` span fields are append-only — using `record()` on a
/// shared span would accumulate duplicate `iteration=N` values, making
/// log filtering unreliable.
///
/// # Phase 4 cleanup
///
/// TODO(Phase 4): Refactor the loop body into `state = step(state, event)?`
/// where `step` is a free function that pattern-matches on the current state
/// and returns the next one. This gives:
/// - Testable individual transitions (no need to run the full loop)
/// - No `#[allow(unused_assignments)]` — state is consumed by the next call
/// - Clean surface for the state-change channel (emit after each `step`)
///
/// The current imperative structure is correct and sufficient for Phase 2.
#[tracing::instrument(skip_all, fields(input_len = message.len()))]
#[allow(unused_variables, unused_assignments, clippy::too_many_lines)]
pub async fn run_loop(
    session: &mut Session,
    message: &str,
    client: &dyn ChatClient,
    registry: &ToolRegistry,
    config: &AgentConfig,
    cancel: CancellationToken,
    gate: &dyn ApprovalGate,
) -> Result<String> {
    session.append_user_message(message);

    // `state` is the observable agent state. The assignments below are
    // intentional architectural stubs: Phase 4 will wire a state-change channel
    // here so the TUI can render live status (Thinking spinner, AwaitingApproval
    // prompt, ExecutingTool progress bar, etc.). The lint is suppressed because
    // these are forward-looking assignments, not dead code.
    #[allow(unused_assignments)]
    let mut state = AgentState::Thinking;
    let mut iterations = 0u32;

    // Stuck-loop detection: track how many consecutive times each
    // (tool_name, arguments) pair produces the same output.
    let mut repetition_counts: HashMap<(String, String), (String, u32)> = HashMap::new();

    loop {
        // Each iteration gets its own span so the `iteration` field is a
        // single value (not accumulated). `tracing` span fields are
        // append-only — `record()` adds rather than replaces — so a
        // per-iteration span is required for reliable filtering.
        let _iter_span = tracing::info_span!("agent_iteration", iteration = iterations).entered();
        // ── Thinking ──────────────────────────────────────────────────────────
        if cancel.is_cancelled() {
            warn!("cancelled");
            return Err(RhoError::Cancelled);
        }

        let response = send_with_retry(session, client, config).await?;

        iterations += 1;
        if iterations > config.max_iterations {
            error!(max = config.max_iterations);
            return Err(RhoError::MaxIterationsExceeded(config.max_iterations));
        }

        match response {
            // ── Idle (terminal) ───────────────────────────────────────────────
            AssistantResponse::Message {
                text,
                reasoning_content,
            } => {
                info!(reply_len = text.len());
                if !reasoning_content.is_empty() {
                    info!(
                        reasoning_len = reasoning_content.len(),
                        "model returned reasoning content with stop finish_reason"
                    );
                }
                state = AgentState::Idle;
                return Ok(if reasoning_content.is_empty() {
                    text
                } else {
                    format!("<thinking>\n{reasoning_content}\n</thinking>\n\n{text}")
                });
            }

            // ── Length-truncated recovery ────────────────────────────────────
            AssistantResponse::LengthTruncated {
                content,
                reasoning_content,
            } => {
                info!(
                    content_len = content.len(),
                    reasoning_len = reasoning_content.len(),
                    "model hit token limit (finish_reason=length)"
                );
                state = AgentState::Thinking;

                // Attempt compaction to free context space, then retry.
                // If compaction fails (too few entries, or nothing to compact),
                // return a user-facing explanation.
                let strategy = crate::session::MechanicalCompactionStrategy::new();
                let budget = session.message_budget();
                let compact_threshold = budget / 4;

                match session
                    .compact_older_than(compact_threshold, &strategy)
                    .await
                {
                    Ok(_compaction_id) => {
                        info!(
                            freed_tokens = compact_threshold,
                            "compacted context after length truncation, retrying"
                        );
                        // Retry: loop back to send with compacted context.
                    }
                    Err(e) => {
                        warn!(error = %e, "compaction failed after length truncation");
                        // Build a user-facing message explaining what happened.
                        let mut explanation = String::from(
                            "The model ran out of tokens before completing its response. \
                             This usually means the conversation grew too large for the \
                             context window. \
                             \n\n",
                        );
                        if !reasoning_content.is_empty() {
                            explanation.push_str(
                                "The model was still thinking (chain-of-thought) and did not \
                                 produce any output before being cut off. Try one of:\
                                 \n  1. Use `/compact` to summarize the conversation and free space\n                                 \n  2. Start a fresh session with `/reset`\n                                 \n  3. Increase the context window in your model server\n",
                            );
                        } else if content.is_empty() {
                            explanation.push_str(
                                "No output was produced. Try `/compact` or `/reset` to continue.\n",
                            );
                        } else {
                            use std::fmt::Write;
                            let _ = write!(
                                explanation,
                                "Partial output (truncated):\n\n{content}\n\n\
                                 Use `/compact` or `/reset` to get a complete response.\n"
                            );
                        }
                        state = AgentState::Idle;
                        return Ok(explanation);
                    }
                }
            }

            AssistantResponse::ToolCalls(calls) => {
                info!(tool_count = calls.len());
                if calls.is_empty() {
                    return Err(RhoError::ProtocolViolation("empty tool_calls".into()));
                }

                // Execute each tool call sequentially. All results are appended
                // before the session is re-sent to the model on the next
                // loop iteration.
                for call in calls {
                    if cancel.is_cancelled() {
                        return Err(RhoError::Cancelled);
                    }

                    let call_id = ToolCallId::new(call.id.to_string());
                    let risk = registry
                        .get_by_name(&call.function.name)
                        .map_or(crate::tool::ToolRisk::Destructive, Tool::risk);

                    // ── AwaitingApproval ──────────────────────────────────────
                    if config
                        .approval_policy
                        .requires_approval(&call.function.name, risk)
                    {
                        state = AgentState::AwaitingApproval;
                        let approved = gate.request_approval(&call, risk).await;
                        if !approved {
                            debug!(tool_name = %call.function.name, action = "denied");
                            let _ = session.append_tool_result(
                                call_id,
                                &ToolResult::error("Tool call denied by user."),
                            );
                            state = AgentState::Thinking;
                            continue;
                        }
                    }

                    // ── ExecutingTool ─────────────────────────────────────────
                    state = AgentState::ExecutingTool;
                    let result = match registry.execute(&call, cancel.clone()).await {
                        Ok(r) => r,
                        Err(e) => {
                            // Persist the error as a tool result so the
                            // conversation history stays valid (every
                            // assistant tool_call must have a matching
                            // tool result). The ? propagation happens
                            // after we write the error to the session.
                            let _ = session
                                .append_tool_result(call_id, &ToolResult::error(format!("{e}")));
                            return Err(e);
                        }
                    };

                    // ── Stuck-loop detection ──────────────────────────────────
                    if config.stuck_loop_threshold > 0 {
                        let key = (
                            call.function.name.to_string(),
                            call.function.arguments.clone(),
                        );
                        let entry = repetition_counts
                            .entry(key)
                            .or_insert_with(|| (String::new(), 0));
                        if entry.0 == result.output {
                            entry.1 += 1;
                        } else {
                            *entry = (result.output.clone(), 1);
                        }
                        if entry.1 >= config.stuck_loop_threshold {
                            warn!(
                                tool_name = %call.function.name,
                                repeat_count = entry.1,
                                "stuck loop detected — same tool call produced \
                                 identical output {} times",
                                entry.1
                            );
                            let nudge = ToolResult::error(format!(
                                "STUCK LOOP DETECTED: you have called `{}` with the \
                                 same arguments {} times and received the same result \
                                 each time. The file on disk has NOT changed between \
                                 calls. You must use edit_file or write_file to modify \
                                 the source code BEFORE running the command again. \
                                 Re-read the file with read_file to see its current \
                                 state, then apply the necessary edits.",
                                call.function.name, entry.1
                            ));
                            let _ = session.append_tool_result(call_id, &nudge);
                            // Reset counter so the model gets another chance.
                            entry.1 = 0;
                            state = AgentState::Thinking;
                            continue;
                        }
                    }

                    let _ = session.append_tool_result(call_id, &result);
                    state = AgentState::Thinking;
                }
            }
        }
    }
}

/// Send `session.send_current` with exponential backoff on transient errors.
///
/// # Errors
///
/// Returns [`RhoError::RetryBudgetExhausted`] when the retry budget is exceeded.
/// Returns any non-retryable [`RhoError`] immediately.
async fn send_with_retry(
    session: &mut Session,
    client: &dyn ChatClient,
    config: &AgentConfig,
) -> Result<AssistantResponse> {
    let mut attempts = 0u32;
    let mut last_error: Option<RhoError> = None;
    loop {
        match session.send_current(client).await {
            Ok(r) => return Ok(r),
            Err(e) => match TransitionError::from_error(e) {
                TransitionError::Retryable(re) if attempts < config.retry_budget => {
                    attempts += 1;
                    debug!(attempt = attempts, max = config.retry_budget, error = %re);
                    last_error = Some(re);
                    let backoff = config
                        .initial_backoff_ms
                        .saturating_mul(1u64 << attempts.min(6));
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
                TransitionError::Retryable(re) => {
                    let last = last_error.unwrap_or(re);
                    warn!(attempts = config.retry_budget, error = %last);
                    return Err(RhoError::RetryBudgetExhausted(
                        config.retry_budget,
                        Box::new(last),
                    ));
                }
                TransitionError::Fatal(e) => return Err(e),
            },
        }
    }
}
