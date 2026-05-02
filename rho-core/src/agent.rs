//! Agent loop state machine.
//!
//! [`run_loop`] drives the conversation until the model stops or a budget is
//! exhausted. [`AgentState`] is the observable state for UIs and tests.
//! [`TransitionError`] classifies errors as retryable or fatal.

use crate::approval::{ApprovalGate, ApprovalPolicy, DefaultApprovalPolicy};
use crate::client::ChatClient;
use crate::conversation::{AssistantResponse, Conversation};
use crate::error::{Result, RhoError};
use crate::newtypes::ToolCallId;
use crate::tool::{CancellationToken, Tool, ToolRegistry, ToolResult};

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
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("max_iterations", &self.max_iterations)
            .field("retry_budget", &self.retry_budget)
            .field("initial_backoff_ms", &self.initial_backoff_ms)
            .field("approval_policy", &"<dyn ApprovalPolicy>")
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
#[allow(unused_variables, unused_assignments)]
pub async fn run_loop(
    conversation: &mut Conversation,
    message: &str,
    client: &dyn ChatClient,
    registry: &ToolRegistry,
    config: &AgentConfig,
    cancel: CancellationToken,
    gate: &dyn ApprovalGate,
) -> Result<String> {
    conversation.push_user_text(message);

    // `state` is the observable agent state. The assignments below are
    // intentional architectural stubs: Phase 4 will wire a state-change channel
    // here so the TUI can render live status (Thinking spinner, AwaitingApproval
    // prompt, ExecutingTool progress bar, etc.). The lint is suppressed because
    // these are forward-looking assignments, not dead code.
    #[allow(unused_assignments)]
    let mut state = AgentState::Thinking;
    let mut iterations = 0u32;

    loop {
        // ── Thinking ──────────────────────────────────────────────────────────
        if cancel.is_cancelled() {
            return Err(RhoError::Unexpected(anyhow::anyhow!("cancelled")));
        }

        let response = send_with_retry(conversation, client, config).await?;

        iterations += 1;
        if iterations > config.max_iterations {
            return Err(RhoError::MaxIterationsExceeded(config.max_iterations));
        }

        match response {
            // ── Idle (terminal) ───────────────────────────────────────────────
            AssistantResponse::Message(text) => {
                state = AgentState::Idle;
                return Ok(text);
            }

            AssistantResponse::ToolCalls(calls) => {
                if calls.is_empty() {
                    return Err(RhoError::Unexpected(anyhow::anyhow!("empty tool_calls")));
                }

                // Execute each tool call sequentially. All results are appended
                // before the conversation is re-sent to the model on the next
                // loop iteration.
                for call in calls {
                    if cancel.is_cancelled() {
                        return Err(RhoError::Unexpected(anyhow::anyhow!("cancelled")));
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
                            conversation.push_tool_result(
                                call_id,
                                &ToolResult::error("Tool call denied by user."),
                            );
                            state = AgentState::Thinking;
                            continue;
                        }
                    }

                    // ── ExecutingTool ─────────────────────────────────────────
                    state = AgentState::ExecutingTool;
                    let result = registry.execute(&call, cancel.clone()).await?;
                    conversation.push_tool_result(call_id, &result);
                    state = AgentState::Thinking;
                }
            }
        }
    }
}

/// Send `conversation.send_current` with exponential backoff on transient errors.
///
/// # Errors
///
/// Returns [`RhoError::RetryBudgetExhausted`] when the retry budget is exceeded.
/// Returns any non-retryable [`RhoError`] immediately.
async fn send_with_retry(
    conversation: &mut Conversation,
    client: &dyn ChatClient,
    config: &AgentConfig,
) -> Result<AssistantResponse> {
    let mut attempts = 0u32;
    let mut last_error: Option<RhoError> = None;
    loop {
        match conversation.send_current(client).await {
            Ok(r) => return Ok(r),
            Err(e) => match TransitionError::from_error(e) {
                TransitionError::Retryable(re) if attempts < config.retry_budget => {
                    attempts += 1;
                    eprintln!(
                        "warn: transient error (attempt {attempts}/{}): {re}",
                        config.retry_budget
                    );
                    last_error = Some(re);
                    let backoff = config
                        .initial_backoff_ms
                        .saturating_mul(1u64 << attempts.min(6));
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
                TransitionError::Retryable(re) => {
                    return Err(RhoError::RetryBudgetExhausted(
                        config.retry_budget,
                        Box::new(last_error.unwrap_or(re)),
                    ));
                }
                TransitionError::Fatal(e) => return Err(e),
            },
        }
    }
}
