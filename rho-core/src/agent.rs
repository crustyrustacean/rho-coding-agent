//! Agent loop state machine.
//!
//! [`run_loop`] drives the conversation until the model stops or a budget is
//! exhausted. Internally, the loop is a state machine where each state
//! transition is handled by a method on the loop context struct. Every transition is
//! independently testable.
//!
//! # State machine
//!
//! ```text
//!                  ┌──────────────────────────────────────────┐
//!                  │                                          │
//!                  ▼                                          │
//!             ┌──────────┐   send to LLM   ┌──────────────┐  │
//!             │ Thinking  │ ─────────────► │ LLM response │  │
//!             └──────────┘                 └──────────────┘  │
//!                  │                    │       │       │     │
//!               text                  tools  truncated      │
//!                  │                    │       │            │
//!                  ▼                    ▼       ▼            │
//!                Done              approval?  compact ──────┘
//!                                  │       │
//!                              yes        no
//!                                  │       │
//!                                  ▼       ▼
//!                        ┌─────────────┐ ┌──────────────┐
//!                        │  Awaiting   │ │  Executing   │
//!                        │  Approval   │ │    Tool      │
//!                        └──────┬──────┘ └──────┬───────┘
//!                           ┌───┴───┐           │
//!                        approved denied         │
//!                           │       │             │
//!                           ▼       └─────────────┼─────┐
//!                     ExecutingTool               │     │
//!                           │                     │     │
//!                           └─────────────────────┘     │
//!                                                       │
//!                  ┌────────────────────────────────────┘
//!                  │
//!                  │  (all calls in batch done)
//!                  ▼
//!             ┌──────────┐
//!             │ Thinking  │ ──► send to LLM again
//!             └──────────┘
//! ```

use crate::approval::{ApprovalDecision, ApprovalGate, ApprovalPolicy, DefaultApprovalPolicy};
use crate::conversation::AssistantResponse;
use crate::error::{Result, RhoError};
use crate::message::ChatMessage;
use crate::message::{ModelToolCall, ToolCallFunction};
use crate::newtypes::{ToolCallId, ToolName};
use crate::session::Session;
use crate::tool::{CancellationToken, Tool, ToolRegistry, ToolResult, ToolRisk};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use thiserror::Error;
use tracing::{debug, error, info, warn};

// ── AgentError ────────────────────────────────────────────────────────────────

/// Errors that can occur during agent loop operations.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The agent loop exceeded its configured iteration limit.
    #[error("agent loop exceeded maximum iterations ({0})")]
    MaxIterationsExceeded(u32),

    /// The agent loop was cancelled by the user or a cancellation token.
    #[error("cancelled")]
    Cancelled,

    /// The model API returned a response that violates the expected protocol.
    ///
    /// For example, the model returned an empty `tool_calls` array or a
    /// streaming outcome that is not yet supported.
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),
}

impl AgentError {
    /// Create a `MaxIterationsExceeded` error.
    pub fn max_iterations_exceeded(limit: u32) -> Self {
        Self::MaxIterationsExceeded(limit)
    }

    /// Create a `ProtocolViolation` error.
    pub fn protocol_violation(message: impl Into<String>) -> Self {
        Self::ProtocolViolation(message.into())
    }
}

/// A specialised `Result` type for agent operations.
pub type AgentResultType<T> = std::result::Result<T, AgentError>;

impl From<AgentError> for crate::error::RhoError {
    fn from(error: AgentError) -> Self {
        crate::error::RhoError::Agent(error)
    }
}

// ── AgentResult (structured run_loop output) ────────────────────────────────

/// Structured result from a single [`run_loop`] invocation.
///
/// Captures everything a consumer (REPL, TUI, bench, headless) needs to
/// understand what the agent did — without implementing custom observers
/// or stream interceptors.
#[derive(Debug, Clone)]
pub struct AgentResult {
    /// The model's final text reply (formatted, with optional reasoning summary).
    pub reply: String,
    /// How many LLM round-trips (iterations) this `run_loop` used.
    pub iterations: u32,
    /// Cumulative token usage for this call (delta, not session-lifetime totals).
    pub usage: TokenUsage,
    /// Every tool call the model requested and what happened, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Wall-clock duration of the entire `run_loop` call.
    pub duration: std::time::Duration,
    /// Why the loop ended.
    pub finish_reason: LoopFinishReason,
    /// Context window stats snapshot taken at the end of the run.
    pub context_stats: crate::session::ContextStats,
}

/// Token usage for a single [`run_loop`] invocation (delta, not session-lifetime).
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Prompt tokens consumed in this call.
    pub input_tokens: u64,
    /// Completion tokens generated in this call.
    pub output_tokens: u64,
    /// Cumulative cost in USD for this call.
    pub total_cost: f64,
    /// Number of LLM requests in this call.
    pub request_count: u32,
}

impl TokenUsage {
    /// Total tokens (input + output).
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// A single tool call's record: what was called, what happened, how long it took.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Tool name (e.g. `"read_file"`, `"run_command"`).
    pub name: String,
    /// Raw arguments JSON string.
    pub arguments: String,
    /// What happened to this tool call.
    pub outcome: ToolCallOutcome,
    /// Wall-clock duration of the tool execution phase.
    ///
    /// `None` for denied/blocked calls (no execution occurred).
    pub duration: Option<std::time::Duration>,
}

/// What happened to a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallOutcome {
    /// Tool executed successfully.
    Success,
    /// Tool returned an error (`non-zero exit`, parse failure, sandbox rejection, etc.).
    Error {
        /// The error output text.
        output: String,
    },
    /// Tool call was denied by the approval gate.
    Denied,
    /// Tool call was blocked by an observer (intercept).
    Blocked {
        /// Why the tool call was blocked.
        reason: String,
    },
}

/// Why [`run_loop`] terminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopFinishReason {
    /// Model produced a normal text reply (`stop` / `end_turn`).
    Stop,
    /// Max iterations exceeded.
    MaxIterations,
    /// User cancelled via `CancellationToken`.
    Cancelled,
    /// Retry budget exhausted on transient errors.
    RetryBudgetExhausted,
    /// The model produced too many consecutive empty responses.
    ConsecutiveEmptyResponses,
}

/// Tagged exit from the agent loop, used internally by [`run_loop`]
/// to classify terminal states before building [`AgentResult`].
enum LoopOutcome {
    /// The loop completed normally.
    Done {
        /// The model's final text reply.
        reply: String,
        /// Why the loop terminated.
        finish_reason: LoopFinishReason,
    },
    /// The loop exited via an error.
    Err(RhoError),
}

// ── CollectingObserver ───────────────────────────────────────────────────────

/// An observer that records tool call events into structured [`ToolCallRecord`]s.
///
/// Always present in [`run_loop`]. Used to populate [`AgentResult::tool_calls`]
/// without requiring consumers to build custom observers.
pub struct CollectingObserver {
    /// Recorded tool call events.
    records: std::sync::Mutex<Vec<ToolCallRecord>>,
    /// Instant when the most recent tool call started executing.
    tool_start: std::sync::Mutex<Option<std::time::Instant>>,
}

impl CollectingObserver {
    /// Create a new collecting observer.
    pub fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(Vec::new()),
            tool_start: std::sync::Mutex::new(None),
        }
    }

    /// Drain all recorded tool calls.
    pub fn into_records(self) -> Vec<ToolCallRecord> {
        self.records.into_inner().unwrap_or_default()
    }

    /// Lock a mutex for recording, returning the guard — or `None` (with a
    /// warning) if the mutex is poisoned. Poison means a prior panic left the
    /// guarded data inconsistent; for an observer — a side-channel that only
    /// records events for later inspection — the right response is to log and
    /// keep running, not crash the agent loop. (Consistent with `into_records`,
    /// which already does `unwrap_or_default`.)
    fn lock_or_warn<T>(mutex: &std::sync::Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
        if let Ok(guard) = mutex.lock() {
            Some(guard)
        } else {
            warn!("CollectingObserver mutex poisoned; dropping event");
            None
        }
    }

    /// Record a tool call that executed (success or error).
    fn record_execution(
        &self,
        name: &str,
        arguments: &str,
        outcome: ToolCallOutcome,
        duration: std::time::Duration,
    ) {
        if let Some(mut records) = Self::lock_or_warn(&self.records) {
            records.push(ToolCallRecord {
                name: name.to_owned(),
                arguments: arguments.to_owned(),
                outcome,
                duration: Some(duration),
            });
        }
    }

    /// Record a tool call that was denied or blocked (no execution).
    fn record_denied(&self, name: &str, arguments: &str, outcome: ToolCallOutcome) {
        if let Some(mut records) = Self::lock_or_warn(&self.records) {
            records.push(ToolCallRecord {
                name: name.to_owned(),
                arguments: arguments.to_owned(),
                outcome,
                duration: None,
            });
        }
    }

    /// Start timing a tool call.
    fn start_tool(&self) {
        if let Some(mut start) = Self::lock_or_warn(&self.tool_start) {
            *start = Some(std::time::Instant::now());
        }
    }

    /// Stop timing and return elapsed duration.
    ///
    /// Degrades gracefully on both failure modes — this observer is a
    /// side-channel and must not crash the agent loop:
    ///   - mutex poisoned → can't read the slot → `Duration::ZERO` (timing lost)
    ///   - no matching `start_tool` → programming error, warned → `Duration::ZERO`
    fn stop_tool(&self) -> std::time::Duration {
        let Some(mut start_slot) = Self::lock_or_warn(&self.tool_start) else {
            return std::time::Duration::ZERO;
        };
        let Some(start) = start_slot.take() else {
            warn!("stop_tool called without a matching start_tool");
            return std::time::Duration::ZERO;
        };
        start.elapsed()
    }
}

impl Default for CollectingObserver {
    fn default() -> Self {
        Self::new()
    }
}

// ── AgentObserver ─────────────────────────────────────────────────────────────

/// Receives live events from the agent loop.
///
/// Implementations can render progress to a REPL, TUI, or test harness.
/// The agent loop calls these methods at every state transition and as
/// stream deltas arrive from the model.
///
/// All methods receive `&str` references (not owned values) so observers
/// can be zero-allocation when they choose to ignore events.
/// The result of a tool-call interception check.
///
/// Returned by [`AgentObserver::on_tool_call_intercept`] to allow extensions
/// to block tool calls before they reach the approval gate or execution.
#[derive(Clone, Debug)]
pub enum InterceptResult {
    /// Block the tool call with a human-readable reason.
    Block {
        /// Why the tool call was blocked.
        reason: String,
    },
    /// Allow the tool call to proceed.
    Allow,
}

/// Receives live events from the agent loop.
///
/// Implementations can render progress to a REPL, TUI, or test harness.
/// The agent loop calls these methods at every state transition and as
/// stream deltas arrive from the model.
///
/// Notification methods are `async` so implementations can perform I/O
/// (e.g., writing to a transport, calling an extension hook) without
/// resorting to `tokio::spawn` fire-and-forget workarounds.
///
/// All methods receive `&str` references (not owned values) so observers
/// can be zero-allocation when they choose to ignore events.
#[async_trait]
pub trait AgentObserver: Send + Sync {
    /// The agent entered a new [`AgentState`].
    async fn on_state_change(&self, _state: AgentState) {}

    /// Incremental text content from the model's streaming response.
    ///
    /// May be called many times per loop iteration as deltas arrive.
    async fn on_text_delta(&self, _delta: &str) {}

    /// Incremental reasoning / chain-of-thought content from the model.
    ///
    /// May be called many times per loop iteration as deltas arrive.
    async fn on_reasoning_delta(&self, _delta: &str) {}

    /// The model requested a tool call with the given name and arguments.
    async fn on_tool_call(&self, _name: &str, _arguments: &str) {}

    /// A tool finished executing and produced this result.
    async fn on_tool_result(&self, _name: &str, _result: &ToolResult) {}

    /// A tool call was denied by the approval gate.
    async fn on_tool_denied(&self, _name: &str) {}

    /// A tool call requires human approval with the given risk level.
    async fn on_approval_requested(&self, _tool_name: &str, _risk: ToolRisk) {}

    /// Intercept a tool call before it reaches the approval gate or execution.
    ///
    /// Called **before** the approval policy check. If any observer returns
    /// [`InterceptResult::Block`], the tool call is denied immediately with
    /// the given reason — the approval gate is never consulted.
    ///
    /// Return `None` (the default) to indicate no opinion (allow). If multiple
    /// observers exist, the first `Block` wins.
    ///
    /// This method is synchronous because it is a pure policy decision;
    /// no implementation should need to perform I/O here.
    fn on_tool_call_intercept(&self, _name: &str, _arguments: &str) -> Option<InterceptResult> {
        None
    }

    /// A model response completed and its token usage was accumulated.
    ///
    /// Fires once per iteration that hit the model (i.e. after each
    /// `route_response`), carrying the per-iteration delta and a live context
    /// snapshot. Observers can use this to render a live context/cost gauge
    /// during long multi-iteration turns, rather than only seeing the final
    /// totals in the final `agent/end` notification.
    async fn on_usage(
        &self,
        _iteration: u32,
        _usage: &IterationUsage,
        _context: &crate::session::ContextStats,
    ) {
    }
}

/// Per-iteration token/cost delta, emitted via [`AgentObserver::on_usage`].
///
/// Aggregates all LLM requests within a single loop iteration (including
/// retries) into one delta. `cost` reflects catalog-derived or
/// provider-reported cost for this iteration; it stays `0.0` when no pricing
/// applied (unknown model / sentinel pricing).
#[derive(Debug, Clone, Default)]
pub struct IterationUsage {
    /// Input (prompt) tokens consumed this iteration.
    pub input_tokens: u64,
    /// Output (completion) tokens produced this iteration.
    pub output_tokens: u64,
    /// Input tokens served from a prompt cache this iteration.
    pub cached_tokens: u64,
    /// Cost in USD for this iteration.
    pub cost: f64,
    /// Number of LLM requests this iteration (>= 1; >1 if retried).
    pub request_count: u32,
}

/// A no-op observer that discards all events.
///
/// Use this as the observer when no live output is needed (tests, benchmarks).
pub struct NopObserver;

impl AgentObserver for NopObserver {}

// ── Observable state ──────────────────────────────────────────────────────────

/// The observable state of the agent loop.
///
/// Exposed to UIs via [`AgentObserver::on_state_change`] so they can render
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

// ── Transition error classification ───────────────────────────────────────────

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
    /// Maximum *loop iterations* before stopping (prevents runaway tool-call
    /// chains). The initial LLM call counts as iteration 1.
    pub max_iterations: u32,
    /// Maximum *retry attempts* on transient errors before giving up. This is
    /// the number of retries, not the total number of attempts (initial + retries
    /// = 1 + `retry_budget`).
    pub retry_budget: u32,
    /// Base backoff in milliseconds. Each retry waits
    /// `initial_backoff_ms * 2^retry_number`, capped at 64× the base.
    pub initial_backoff_ms: u64,
    /// Policy that decides whether a tool call needs human approval.
    pub approval_policy: Box<dyn ApprovalPolicy>,
    /// Number of times a tool call with identical (name, arguments, output)
    /// may repeat before the agent injects a stuck-loop nudge. Set to 0 to
    /// disable stuck-loop detection.
    pub stuck_loop_threshold: u32,
    /// Maximum number of consecutive empty model responses before aborting
    /// with an error. Empty responses consume iterations without making
    /// progress. Set to 0 to allow unlimited empty retries (not recommended).
    pub max_consecutive_empty: u32,
    /// Whether to display full chain-of-thought reasoning in the output.
    /// When `false`, shows a one-line summary instead.
    pub show_reasoning: bool,
    /// Context utilization percentage (0–100) at which the agent loop
    /// automatically compacts older entries to free context space.
    /// Compaction runs proactively *before* eviction is needed.
    /// Set to 0 to disable (default).
    pub auto_compact_threshold: u8,
    /// Compaction mode: `"mechanical"` (default) or `"llm"`.
    /// When `"llm"`, the agent uses the LLM to generate narrative summaries
    /// during compaction.
    pub compaction_mode: String,
    /// Maximum seconds to wait for the first stream event before aborting
    /// the attempt and retrying. 0 disables the timeout.
    pub first_token_timeout_secs: u64,
    /// Maximum seconds allowed between consecutive stream events before
    /// aborting the attempt and retrying. 0 disables the timeout.
    pub stream_idle_timeout_secs: u64,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("max_iterations", &self.max_iterations)
            .field("retry_budget", &self.retry_budget)
            .field("initial_backoff_ms", &self.initial_backoff_ms)
            .field("approval_policy", &"<dyn ApprovalPolicy>")
            .field("stuck_loop_threshold", &self.stuck_loop_threshold)
            .field("max_consecutive_empty", &self.max_consecutive_empty)
            .field("show_reasoning", &self.show_reasoning)
            .field("auto_compact_threshold", &self.auto_compact_threshold)
            .field("compaction_mode", &self.compaction_mode)
            .field("first_token_timeout_secs", &self.first_token_timeout_secs)
            .field("stream_idle_timeout_secs", &self.stream_idle_timeout_secs)
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
            max_consecutive_empty: 5,
            show_reasoning: false,
            auto_compact_threshold: 0,
            compaction_mode: "mechanical".to_owned(),
            first_token_timeout_secs: 90,
            stream_idle_timeout_secs: 60,
        }
    }
}

impl AgentConfig {
    /// Create an `AgentConfig` from a [`RhoConfig`], using the config-driven
    /// approval policy.
    ///
    /// [`RhoConfig`]: crate::config::RhoConfig
    pub fn from_config(config: &crate::config::RhoConfig) -> Self {
        use crate::approval::ConfigApprovalPolicy;
        Self {
            max_iterations: config.agent.max_iterations,
            retry_budget: config.agent.retry_budget,
            initial_backoff_ms: config.agent.initial_backoff_ms,
            approval_policy: Box::new(ConfigApprovalPolicy::new(config)),
            stuck_loop_threshold: config.agent.stuck_loop_threshold,
            max_consecutive_empty: config.agent.max_consecutive_empty,
            show_reasoning: config.agent.show_reasoning,
            auto_compact_threshold: config.agent.auto_compact_threshold,
            compaction_mode: config.agent.compaction_mode.clone(),
            first_token_timeout_secs: config.agent.first_token_timeout_secs,
            stream_idle_timeout_secs: config.agent.stream_idle_timeout_secs,
        }
    }
}

// ── LoopParams ───────────────────────────────────────────────────────────────

/// Stable context passed to every invocation of [`run_loop`].
///
/// Bundles the six parameters that don't change between calls so `run_loop`
/// takes three arguments instead of eight.
///
/// # Examples
///
/// ```ignore
/// let params = LoopParams {
///     client: &client,
///     registry: &registry,
///     config: &agent_config,
///     cancel: cancel_token,
///     gate: &approval_gate,
///     observer: &observer,
///     compaction_client: None,
///     steering: None,
/// };
/// let reply = run_loop(&mut session, "hello", &params).await?;
/// ```
/// Source of mid-turn steering messages.
///
/// Drained at the seam between tool-batch completion and the next thinking
/// step. Synchronous (draining a queue needs no `await`), which keeps the
/// trait dyn-safe without `#[async_trait]`.
pub trait SteeringSource: Send + Sync {
    /// Drain and return all currently-queued steering messages, in order.
    fn drain(&self) -> Vec<String>;
}

/// A thread-safe, clonable backing store for [`SteeringSource`].
///
/// Intended for the RPC layer: the reader task pushes incoming steering
/// messages via [`SteeringQueue::push`], and the agent loop drains them at
/// the tool-batch seam through the [`SteeringSource`] impl.
#[derive(Debug, Clone, Default)]
pub struct SteeringQueue {
    /// Lock-protected buffer of pending steering messages.
    inner: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
}

impl SteeringQueue {
    /// Create an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a steering message onto the queue (called by the reader task).
    ///
    /// Silently dropped if the lock is poisoned (a holder panicked) — poison is
    /// a catastrophic state where losing a steer is the least concern.
    pub fn push(&self, msg: String) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.push_back(msg);
        }
    }
}

impl SteeringSource for SteeringQueue {
    fn drain(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|mut guard| guard.drain(..).collect())
            .unwrap_or_default()
    }
}

pub struct LoopParams<'a> {
    /// The LLM service that talks to the model.
    pub client: &'a dyn rho_ai::LlmService,
    /// Maps tool names to implementations.
    pub registry: &'a ToolRegistry,
    /// Agent loop configuration (max iterations, retry budget, etc.).
    pub config: &'a AgentConfig,
    /// Cooperative cancellation token (Ctrl-C).
    pub cancel: CancellationToken,
    /// The approval gate (prompts user to approve tool calls).
    pub gate: &'a dyn ApprovalGate,
    /// UI callbacks for state changes and streaming deltas.
    pub observer: &'a dyn AgentObserver,
    /// Optional LLM client for LLM-powered compaction.
    ///
    /// When set and `config.compaction_mode == "llm"`, compaction uses this
    /// client to generate narrative summaries. The caller should provide an
    /// `Arc` wrapping the same provider used for the main loop.
    pub compaction_client: Option<std::sync::Arc<dyn rho_ai::LlmService>>,

    /// Optional source of mid-turn steering messages — drained between
    /// tool-batch completion and the next thinking step (see [`SteeringSource`]).
    pub steering: Option<&'a dyn SteeringSource>,
}

// ── Internal state machine ────────────────────────────────────────────────────

/// Internal state machine states.
///
/// Unlike [`AgentState`] (which is observable by the UI), these states carry
/// the data needed for the next transition. Each variant holds only what its
/// handler needs.
enum State {
    /// Send the conversation to the LLM and route the response.
    Thinking,

    /// Waiting for user approval before executing a tool call.
    AwaitingApproval {
        /// The tool call awaiting approval.
        call: ModelToolCall,
        /// Tool calls to process after this one.
        remaining: Vec<ModelToolCall>,
    },

    /// Executing a tool call.
    ExecutingTool {
        /// The tool call being executed.
        call: ModelToolCall,
        /// Tool calls to process after this one.
        remaining: Vec<ModelToolCall>,
    },

    /// Terminal state — the loop is done.
    Outcome(LoopOutcome),
}

/// Mutable context shared across all state transitions.
///
/// Takes the stable [`LoopParams`] and adds per-invocation mutable state
/// (session, repetition tracking, iteration count).
struct LoopContext<'a> {
    /// The conversation session (tree-shaped, persisted).
    session: &'a mut Session,
    /// Stable context (client, registry, config, cancel, gate, observer).
    params: &'a LoopParams<'a>,
    /// Stuck-loop detection: maps `(tool_name, arguments)` → `(last_output, count)`.
    repetition_counts: HashMap<(String, String), (String, u32)>,
    /// Number of completed LLM round-trips in this `run_loop` invocation.
    iterations: u32,
    /// Current session phase (Exploration, Execution, Verification, Conclusion).
    phase: crate::session::phase::SessionPhase,
    /// Whether any edit/write tools have been executed in this loop.
    has_had_edits: bool,
    /// Number of consecutive empty model responses (for abort threshold).
    consecutive_empty_count: u32,
    /// Records tool calls for the structured `AgentResult`.
    collector: &'a CollectingObserver,
}

impl LoopContext<'_> {
    /// Create the appropriate compaction strategy based on config.
    ///
    /// Returns `MechanicalCompactionStrategy` when `compaction_mode == "mechanical"`
    /// (default), or `LlmCompactionStrategy` when `compaction_mode == "llm"` and a
    /// compaction client is available. Falls back to mechanical if the LLM client
    /// is missing.
    fn make_compaction_strategy(&self) -> Box<dyn crate::session::CompactionStrategy> {
        if self.params.config.compaction_mode == "llm" {
            if let Some(ref client) = self.params.compaction_client {
                return Box::new(
                    crate::session::LlmCompactionStrategy::new(
                        client.clone(),
                        self.session.model.clone(),
                    )
                    .with_max_tokens(512),
                );
            }
            warn!(
                "compaction_mode is 'llm' but no compaction_client provided, falling back to mechanical"
            );
        }
        Box::new(crate::session::MechanicalCompactionStrategy::new())
    }

    /// Execute one state transition and return the next state.
    async fn step(&mut self, state: State) -> Result<State> {
        match state {
            State::Thinking => self.handle_thinking().await,
            State::AwaitingApproval { call, remaining } => {
                self.handle_approval(call, remaining).await
            }
            State::ExecutingTool { call, remaining } => {
                self.handle_execution(call, remaining).await
            }
            State::Outcome(_) => unreachable!("Outcome is terminal"),
        }
    }

    // ── Thinking ─────────────────────────────────────────────────────────

    /// Send the conversation to the LLM and route the response.
    async fn handle_thinking(&mut self) -> Result<State> {
        if self.params.cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }

        // Snapshot usage before the model call so we can emit a per-iteration
        // delta. `send_with_retry` → `route_response` accumulates into the
        // session, so the difference is this iteration's contribution
        // (including any retries within the iteration).
        let usage_before = self.session.api_usage().clone();

        let response = self.send_with_retry().await?;
        self.iterations += 1;

        // Emit a live usage tick: the per-iteration delta plus a fresh
        // context snapshot. Observers (REPL/TUI) can render a context/cost
        // gauge during long multi-iteration turns from this alone, without
        // a `getSessionStats` round-trip per step.
        let delta = {
            let after = self.session.api_usage();
            IterationUsage {
                input_tokens: after.total_input_tokens - usage_before.total_input_tokens,
                output_tokens: after.total_output_tokens - usage_before.total_output_tokens,
                cached_tokens: after.total_cached_tokens - usage_before.total_cached_tokens,
                cost: after.total_cost - usage_before.total_cost,
                request_count: after.request_count - usage_before.request_count,
            }
        };
        let context = self.session.context_stats();
        self.params
            .observer
            .on_usage(self.iterations, &delta, &context)
            .await;

        if self.iterations > self.params.config.max_iterations {
            error!(max = self.params.config.max_iterations);
            return Err(
                AgentError::MaxIterationsExceeded(self.params.config.max_iterations).into(),
            );
        }

        match response {
            // ── Terminal: text reply ─────────────────────────────────────
            AssistantResponse::Message {
                text,
                reasoning_content,
            } => {
                // Model produced actual output — reset empty counter.
                self.consecutive_empty_count = 0;
                info!(reply_len = text.len());
                if !reasoning_content.is_empty() {
                    info!(
                        reasoning_len = reasoning_content.len(),
                        "model returned reasoning content with stop finish_reason"
                    );
                }
                self.phase = crate::session::phase::SessionPhase::Conclusion;
                self.params.observer.on_state_change(AgentState::Idle).await;
                let reply = self.format_reply(text, &reasoning_content);
                Ok(State::Outcome(LoopOutcome::Done {
                    reply,
                    finish_reason: LoopFinishReason::Stop,
                }))
            }

            // ── Length-truncated recovery ────────────────────────────────
            AssistantResponse::LengthTruncated {
                content,
                reasoning_content,
            } => self.handle_truncation(content, reasoning_content).await,

            // ── Tool calls ───────────────────────────────────────────────
            AssistantResponse::ToolCalls(calls) => {
                info!(tool_count = calls.len());
                if calls.is_empty() {
                    return Err(
                        AgentError::ProtocolViolation("empty tool_calls".to_string()).into(),
                    );
                }
                // Model produced tool calls — reset empty counter.
                self.consecutive_empty_count = 0;
                Ok(self.classify_first_call(calls).await)
            }
        }
    }

    // ── AwaitingApproval ─────────────────────────────────────────────────

    /// Ask the user to approve (or deny) a tool call.
    async fn handle_approval(
        &mut self,
        call: ModelToolCall,
        remaining: Vec<ModelToolCall>,
    ) -> Result<State> {
        let risk = self
            .params
            .registry
            .get_by_name(&call.function.name)
            .map_or(ToolRisk::Destructive, Tool::risk);

        match self.params.gate.request_approval(&call, risk).await {
            ApprovalDecision::Approved => {
                // Approved — transition to executing.
                self.params
                    .observer
                    .on_state_change(AgentState::ExecutingTool)
                    .await;
                Ok(State::ExecutingTool { call, remaining })
            }
            ApprovalDecision::Redirect { message } => {
                // User denied and provided alternative instructions.
                // Inject as a user message and return to Thinking so the
                // model can re-plan.
                debug!(tool_name = %call.function.name, action = "redirected");
                self.collector.record_denied(
                    &call.function.name,
                    &call.function.arguments,
                    ToolCallOutcome::Denied,
                );
                let call_id = ToolCallId::new(call.id.to_string());
                let _ = self.session.append_tool_result(
                    call_id,
                    &ToolResult::error("Tool call redirected by user."),
                );
                self.session.append_user_message(&message);
                Ok(State::Thinking)
            }
            ApprovalDecision::Denied => {
                // User denied with no redirect — record and advance.
                debug!(tool_name = %call.function.name, action = "denied");
                self.collector.record_denied(
                    &call.function.name,
                    &call.function.arguments,
                    ToolCallOutcome::Denied,
                );
                self.params
                    .observer
                    .on_tool_denied(&call.function.name)
                    .await;
                let call_id = ToolCallId::new(call.id.to_string());
                let _ = self
                    .session
                    .append_tool_result(call_id, &ToolResult::error("Tool call denied by user."));
                Ok(self.advance_to_next_call(remaining).await)
            }
        }
    }

    // ── ExecutingTool ────────────────────────────────────────────────────

    /// Return the auto-compact threshold, or `None` if disabled.
    fn auto_compact_threshold(&self) -> Option<u8> {
        let t = self.params.config.auto_compact_threshold;
        if t == 0 { None } else { Some(t) }
    }
    /// Execute a single tool call and handle the result (including stuck-loop
    /// detection).
    async fn handle_execution(
        &mut self,
        call: ModelToolCall,
        remaining: Vec<ModelToolCall>,
    ) -> Result<State> {
        if self.params.cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }

        let call_id = ToolCallId::new(call.id.to_string());

        self.collector.start_tool();
        let result = match self
            .params
            .registry
            .execute(&call, self.params.cancel.clone())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Feed the error back to the model as a tool result so it can
                // see what went wrong and retry.
                warn!(error = %e, "tool execution failed, feeding error back to model");
                let msg = self.format_tool_error(&e);
                let duration = self.collector.stop_tool();
                self.collector.record_execution(
                    &call.function.name,
                    &call.function.arguments,
                    ToolCallOutcome::Error {
                        output: msg.clone(),
                    },
                    duration,
                );
                let _ = self
                    .session
                    .append_tool_result(call_id, &ToolResult::error(msg));
                return Ok(self.advance_to_next_call(remaining).await);
            }
        };

        // Stuck-loop detection.
        if self.params.config.stuck_loop_threshold > 0
            && let Some(nudge) = self.check_stuck_loop(&call, &result)
        {
            let _ = self.session.append_tool_result(call_id, &nudge);
            return Ok(self.advance_to_next_call(remaining).await);
        }

        let duration = self.collector.stop_tool();
        let outcome = if result.is_error {
            ToolCallOutcome::Error {
                output: result.output.clone(),
            }
        } else {
            ToolCallOutcome::Success
        };
        self.collector.record_execution(
            &call.function.name,
            &call.function.arguments,
            outcome,
            duration,
        );

        self.params
            .observer
            .on_tool_result(&call.function.name, &result)
            .await;
        let _ = self.session.append_tool_result(call_id, &result);

        // Phase detection: update session phase based on tool name.
        self.phase = crate::session::phase::transition_phase(
            self.phase,
            &call.function.name,
            self.has_had_edits,
        );
        if matches!(self.phase, crate::session::phase::SessionPhase::Execution) {
            self.has_had_edits = true;
        }
        info!(phase = %self.phase, tool = %call.function.name, "phase updated");

        // Auto-compact: proactively compact older entries when utilization
        // crosses the auto-compact threshold.
        if let Some(threshold) = self.auto_compact_threshold() {
            let stats = self.session.context_stats();
            if stats.utilization_percent() >= threshold {
                let strategy = self.make_compaction_strategy();
                let budget = self.session.message_budget();
                let compact_threshold = budget / 4;
                match self
                    .session
                    .compact_older_than(compact_threshold, strategy.as_ref())
                    .await
                {
                    Ok(_) => {
                        info!(
                            utilization = %stats.utilization_percent(),
                            compact_threshold,
                            "auto-compacted context after tool execution"
                        );
                    }
                    Err(e) => {
                        warn!(error = %e, "auto-compact failed after tool execution");
                    }
                }
            }
        }

        Ok(self.advance_to_next_call(remaining).await)
    }

    // ── Truncation handling ──────────────────────────────────────────────

    /// Handle a length-truncated response by injecting a nudge (empty output)
    /// or attempting compaction.
    async fn handle_truncation(
        &mut self,
        content: String,
        reasoning_content: String,
    ) -> Result<State> {
        info!(
            content_len = content.len(),
            reasoning_len = reasoning_content.len(),
            "model hit token limit (finish_reason=length)"
        );

        // When both content and reasoning are empty, the model produced
        // nothing at all. This is common with reasoning models (llama.cpp,
        // LM Studio) that report "stop" instead of "length" when the
        // completion budget is exhausted during thinking.
        if content.is_empty() && reasoning_content.is_empty() {
            self.consecutive_empty_count += 1;
            let max = self.params.config.max_consecutive_empty;
            if max > 0 && self.consecutive_empty_count >= max {
                let count = self.consecutive_empty_count;
                error!(
                    count,
                    max, "aborting: model produced {count} consecutive empty responses"
                );
                self.params.observer.on_state_change(AgentState::Idle).await;
                return Ok(State::Outcome(LoopOutcome::Done {
                    reply: "The model returned empty responses \
                        consecutively and was unable to continue. \
                        This may indicate the model is not functioning \
                        correctly or is too small for the task."
                        .to_owned(),
                    finish_reason: LoopFinishReason::ConsecutiveEmptyResponses,
                }));
            }
            warn!(
                count = self.consecutive_empty_count,
                "model produced empty response, injecting nudge and retrying"
            );
            self.session.append_user_message(
                "Your previous response was empty. Please provide a \
                 tool call or text response and try again.",
            );
            return Ok(State::Thinking);
        }

        // Attempt compaction to free context space, then retry.
        self.consecutive_empty_count = 0;
        let strategy = self.make_compaction_strategy();
        let budget = self.session.message_budget();
        let compact_threshold = budget / 4;

        match self
            .session
            .compact_older_than(compact_threshold, strategy.as_ref())
            .await
        {
            Ok(_) => {
                info!(
                    freed_tokens = compact_threshold,
                    "compacted context after length truncation, retrying"
                );
                Ok(State::Thinking)
            }
            Err(e) => {
                warn!(error = %e, "compaction failed after length truncation");
                let explanation = Self::format_truncation_explanation(&content, &reasoning_content);
                self.params.observer.on_state_change(AgentState::Idle).await;
                Ok(State::Outcome(LoopOutcome::Done {
                    reply: explanation,
                    finish_reason: LoopFinishReason::Stop,
                }))
            }
        }
    }

    // ── Call classification helpers ──────────────────────────────────────

    /// From a batch of tool calls, classify the first one (approval or
    /// execute) and stash the rest.
    async fn classify_first_call(&mut self, calls: Vec<ModelToolCall>) -> State {
        // SAFETY: callers check `calls.is_empty()` before calling this.
        assert!(!calls.is_empty(), "calls is non-empty");
        self.advance_to_next_call(calls).await
    }

    /// After processing one tool call, determine the state for the next one
    /// (or go back to [`State::Thinking`] if none remain).
    ///
    /// Loops internally so that blocked tool calls (intercepted by an
    /// observer) are skipped without recursion.
    async fn advance_to_next_call(&mut self, mut remaining: Vec<ModelToolCall>) -> State {
        while let Some(next) = remaining.first().cloned() {
            remaining.remove(0);
            let state = self.classify_call_inner(next, &mut remaining).await;
            // classify_call_inner returns None when the call was blocked
            // (already handled) and we should try the next one.
            if let Some(state) = state {
                return state;
            }
        }
        // Drain any queued steering messages and inject them as user messages,
        // so the next thinking step re-plans with the user's mid-turn input.
        if let Some(src) = self.params.steering {
            for msg in src.drain() {
                self.session.append_user_message(&msg);
            }
        }
        self.params
            .observer
            .on_state_change(AgentState::Thinking)
            .await;
        State::Thinking
    }

    /// Inner classification logic for a single tool call.
    ///
    /// Returns `None` if the call was blocked by an observer (caller should
    /// advance to the next call), or `Some(state)` for actionable states.
    async fn classify_call_inner(
        &mut self,
        call: ModelToolCall,
        remaining: &mut Vec<ModelToolCall>,
    ) -> Option<State> {
        // Check observers for interception.
        if let Some(InterceptResult::Block { reason }) = self
            .params
            .observer
            .on_tool_call_intercept(&call.function.name, &call.function.arguments)
        {
            debug!(tool_name = %call.function.name, %reason, "tool call blocked by observer");
            self.collector.record_denied(
                &call.function.name,
                &call.function.arguments,
                ToolCallOutcome::Blocked {
                    reason: reason.clone(),
                },
            );
            self.params
                .observer
                .on_tool_denied(&call.function.name)
                .await;
            let call_id = ToolCallId::new(call.id.to_string());
            let _ = self.session.append_tool_result(
                call_id,
                &ToolResult::error(format!("Tool call blocked: {reason}")),
            );
            return None;
        }

        let risk = self
            .params
            .registry
            .get_by_name(&call.function.name)
            .map_or(ToolRisk::Destructive, Tool::risk);

        self.params
            .observer
            .on_tool_call(&call.function.name, &call.function.arguments)
            .await;

        if self
            .params
            .config
            .approval_policy
            .requires_approval(&call.function.name, risk)
        {
            self.params
                .observer
                .on_state_change(AgentState::AwaitingApproval)
                .await;
            self.params
                .observer
                .on_approval_requested(&call.function.name, risk)
                .await;
            let remaining = std::mem::take(remaining);
            Some(State::AwaitingApproval { call, remaining })
        } else {
            self.params
                .observer
                .on_state_change(AgentState::ExecutingTool)
                .await;
            let remaining = std::mem::take(remaining);
            Some(State::ExecutingTool { call, remaining })
        }
    }

    // ── Stuck-loop detection ─────────────────────────────────────────────

    /// Check for a stuck loop (repeated identical tool calls).
    ///
    /// Returns a nudge [`ToolResult`] if a stuck loop is detected, or `None`.
    fn check_stuck_loop(
        &mut self,
        call: &ModelToolCall,
        result: &ToolResult,
    ) -> Option<ToolResult> {
        let key = (
            call.function.name.to_string(),
            call.function.arguments.clone(),
        );
        let entry = self
            .repetition_counts
            .entry(key)
            .or_insert_with(|| (String::new(), 0));

        if entry.0 == result.output {
            entry.1 += 1;
        } else {
            *entry = (result.output.clone(), 1);
        }

        if entry.1 >= self.params.config.stuck_loop_threshold {
            let count = entry.1;
            warn!(
                tool_name = %call.function.name,
                repeat_count = count,
                "stuck loop detected — same tool call produced \
                 identical output {count} times",
            );
            // Reset counter so the model gets another chance.
            entry.1 = 0;

            // Tailor the nudge based on the tool that's stuck.
            let nudge = if call.function.name == ToolName::from("run_command") {
                format!(
                    "STUCK LOOP DETECTED: you have called `run_command` {count} times \
                     with the same arguments and received the same result each time. \
                     The command is not producing different output on retry. \
                     \n\
                     Common causes: \
                     - Wrong working directory: use the `cwd` parameter to run in a \
                       subdirectory (e.g. `run_command(cwd=\"<subdir>\", command=\"<cmd>\")`). \
                       Do not use `cd` or `Set-Location` — it does not persist. \
                     - The command needs a file edit first: use `edit_file` or `write_file` \
                       to change source code before re-running. \
                     - The command itself is wrong: re-read the error output and try a \
                       different approach."
                )
            } else {
                format!(
                    "STUCK LOOP DETECTED: you have called `{}` with the \
                     same arguments {count} times and received the same result \
                     each time. The file on disk has NOT changed between \
                     calls. You must use edit_file or write_file to modify \
                     the source code BEFORE running the command again. \
                     Re-read the file with read_file to see its current \
                     state, then apply the necessary edits.",
                    call.function.name,
                )
            };
            Some(ToolResult::error(nudge))
        } else {
            None
        }
    }

    // ── Formatting helpers ───────────────────────────────────────────────

    /// Format a tool error for the model, adding actionable hints for
    /// common failure modes (e.g. sandbox path resolution).
    fn format_tool_error(&self, error: &crate::error::RhoError) -> String {
        let base = format!("{error}");
        if let crate::error::RhoError::Sandbox(_) = error {
            // Sandbox path resolution failures are the #1 reason small models
            // get stuck in retry loops. Append a hint so the model can
            // self-correct without wasting iterations.
            let cwd = self.session.header().cwd.to_string_lossy();
            format!(
                "{base}\n\nHint: paths are resolved relative to the sandbox root ({cwd}). \
                    Use a path relative to that root (e.g. \"rho-ai/src/openai.rs\"), \
                    not a shell-style path. Do not use \"cd\" — `read_file` and `edit_file` \
                    are not shell commands."
            )
        } else {
            base
        }
    }

    /// Format the final reply, optionally including reasoning content.
    fn format_reply(&self, text: String, reasoning_content: &str) -> String {
        if reasoning_content.is_empty() {
            text
        } else if self.params.config.show_reasoning {
            format!("<thinking>\n{reasoning_content}\n</thinking>\n\n{text}")
        } else {
            let reasoning_tokens = reasoning_content.len() / 4;
            format!("[reasoning: ~{reasoning_tokens} tokens]\n\n{text}")
        }
    }

    /// Format a user-facing explanation for a length-truncated response.
    fn format_truncation_explanation(content: &str, reasoning_content: &str) -> String {
        let mut explanation = String::from(
            "The model ran out of tokens before completing its response. \
             This usually means the conversation grew too large for the \
             context window. \
             \n\n",
        );
        if reasoning_content.is_empty() {
            use std::fmt::Write;
            let _ = write!(
                explanation,
                "Partial output (truncated):\n\n{content}\n\n\
                 Use `/compact` or `/reset` to get a complete response.\n"
            );
        } else {
            explanation.push_str(
                "The model was still thinking (chain-of-thought) and did not \
                 produce any output before being cut off. Try one of:\
                 \n  1. Use `/compact` to summarize the conversation and free space\
                 \n\n  2. Start a fresh session with `/reset`\
                 \n\n  3. Increase the context window in your model server\n",
            );
        }
        explanation
    }

    // ── LLM communication ───────────────────────────────────────────────

    /// Send a streaming chat request with exponential backoff on transient
    /// errors.
    async fn send_with_retry(&mut self) -> Result<AssistantResponse> {
        let mut attempts = 0u32;
        let mut last_error: Option<RhoError> = None;
        loop {
            match self.send_streaming().await {
                Ok(r) => return Ok(r),
                Err(e) => match TransitionError::from_error(e) {
                    TransitionError::Retryable(re)
                        if attempts < self.params.config.retry_budget =>
                    {
                        attempts += 1;
                        debug!(
                            attempt = attempts,
                            max = self.params.config.retry_budget,
                            error = %re
                        );
                        last_error = Some(re);
                        let backoff = self
                            .params
                            .config
                            .initial_backoff_ms
                            .saturating_mul(1u64 << attempts.min(6));
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                    }
                    TransitionError::Retryable(re) => {
                        let last = last_error.unwrap_or(re);
                        warn!(attempts = self.params.config.retry_budget, error = %last);
                        return Err(RhoError::RetryBudgetExhausted(
                            self.params.config.retry_budget,
                            Box::new(last),
                        ));
                    }
                    TransitionError::Fatal(e) => return Err(e),
                },
            }
        }
    }

    /// Execute a single streaming request, consume the stream, and build an
    /// [`AssistantResponse`].
    async fn send_streaming(&mut self) -> Result<AssistantResponse> {
        self.session.prepare_context();
        let llm_request = build_llm_request(self.session);

        info!(
            "creating streaming request: model={}, messages={}, tools={}",
            llm_request.model,
            llm_request.messages.len(),
            llm_request.tools.len()
        );
        debug!(
            "request: model={}, messages={}, tools={}",
            llm_request.model,
            llm_request.messages.len(),
            llm_request.tools.len()
        );

        let event_stream = self
            .params
            .client
            .chat_stream(llm_request)
            .await
            .map_err(|e| {
                crate::error::RhoError::Client(crate::client::error::ClientError::from(e))
            })?;

        let timeouts = StreamTimeouts {
            first_token: secs_to_duration(self.params.config.first_token_timeout_secs),
            idle: secs_to_duration(self.params.config.stream_idle_timeout_secs),
        };
        let events = consume_stream(event_stream, self.params.observer, timeouts).await?;
        debug!("Stream ended with {} events", events.len());

        let acc = rho_ai::StreamEvent::accumulate(&events);
        route_response(&acc, self.session)
    }
}

// ── run_loop ──────────────────────────────────────────────────────────────────

/// Run the agent loop until the model produces a final text reply.
///
/// Appends `message` as a user turn, then cycles through the internal
/// state machine:
///
/// `Thinking` → `AwaitingApproval`? → `ExecutingTool` → `Thinking` → … → `Done`
///
/// # Errors
///
/// - [`AgentError::MaxIterationsExceeded`] — loop ran past `config.max_iterations`
/// - [`RhoError::RetryBudgetExhausted`] — transient error retried too many times
/// - Any fatal error from the client or tool registry
#[tracing::instrument(skip_all, fields(input_len = message.len()))]
pub async fn run_loop(
    session: &mut Session,
    message: &str,
    params: &LoopParams<'_>,
) -> Result<AgentResult> {
    info!("run_loop called with message: {}", message);
    let start = std::time::Instant::now();
    let usage_before = session.api_usage().clone();
    let collector = CollectingObserver::new();

    session.append_user_message(message);

    let mut ctx = LoopContext {
        session,
        params,
        repetition_counts: HashMap::new(),
        iterations: 0,
        phase: crate::session::phase::SessionPhase::default(),
        has_had_edits: false,
        consecutive_empty_count: 0,
        collector: &collector,
    };

    let mut state = State::Thinking;
    params.observer.on_state_change(AgentState::Thinking).await;

    let outcome = loop {
        let _iter_span =
            tracing::info_span!("agent_iteration", iteration = ctx.iterations).entered();
        match ctx.step(state).await {
            Ok(next_state) => {
                if let State::Outcome(outcome) = next_state {
                    break outcome;
                }
                state = next_state;
            }
            Err(e) => break LoopOutcome::Err(e),
        }
    };

    // Drop ctx to release the mutable borrow on session.
    let iterations = ctx.iterations;
    drop(ctx);

    match outcome {
        LoopOutcome::Done {
            reply,
            finish_reason,
        } => {
            let usage_after = session.api_usage();
            Ok(AgentResult {
                reply,
                iterations,
                usage: TokenUsage {
                    input_tokens: usage_after.total_input_tokens - usage_before.total_input_tokens,
                    output_tokens: usage_after.total_output_tokens
                        - usage_before.total_output_tokens,
                    total_cost: usage_after.total_cost - usage_before.total_cost,
                    request_count: usage_after.request_count - usage_before.request_count,
                },
                tool_calls: collector.into_records(),
                duration: start.elapsed(),
                finish_reason,
                context_stats: session.context_stats(),
            })
        }
        LoopOutcome::Err(e) => {
            record_turn_error(session, &e);
            Err(classify_loop_error(e))
        }
    }
}

/// Classify a [`RhoError`] from the loop into an appropriate [`RhoError`]
/// for the error path.
fn classify_loop_error(e: RhoError) -> RhoError {
    e
}

/// Record a terminal agent-loop error on the session as an `Attached`
/// marker entry so failures leave a diagnostic trace on disk.
///
/// Without this, a turn that fails terminally (e.g. retry budget exhausted
/// after repeated stream timeouts) vanishes — the conversation log shows the
/// user message with no reply and no explanation. The marker uses the
/// `rho.turn_error.v1` kind and stores a coarse category plus the error
/// message as JSON, queryable from the session log.
///
/// Cancellation ([`AgentError::Cancelled`]) is user-initiated and is not
/// recorded.
fn record_turn_error(session: &mut Session, error: &RhoError) {
    if matches!(error, RhoError::Agent(AgentError::Cancelled)) {
        return;
    }
    let category = match error {
        RhoError::Client(_) | RhoError::RetryBudgetExhausted(_, _) => "client",
        RhoError::Agent(_) => "agent",
        RhoError::Session(_) => "session",
        RhoError::Sandbox(_) => "sandbox",
        RhoError::ToolNotFound(_) | RhoError::Tool(_) => "tool",
    };
    session.append_custom_state(
        "rho.turn_error.v1".to_owned(),
        serde_json::json!({ "category": category, "error": error.to_string() }),
    );
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Build an [`LlmRequest`] from the current session state.
///
/// Converts session messages to [`LlmMessage`] via [`ChatMessage::to_llm_message`],
/// maps tool schemas to [`ToolDefinition`](rho_ai::ToolDefinition), and sets
/// `max_tokens` from the session's token budget.
fn build_llm_request(session: &Session) -> rho_ai::LlmRequest {
    let fitted = session.path_messages();
    let llm_messages: Vec<rho_ai::LlmMessage> =
        fitted.iter().map(ChatMessage::to_llm_message).collect();

    rho_ai::LlmRequest {
        model: session.model.clone(),
        messages: llm_messages,
        tools: session.tools.clone(),
        max_tokens: Some(session.token_budget().completion_reserve),
        reasoning_effort: session.reasoning_effort.clone(),
    }
}

/// Build typed tool calls from streaming accumulation.
fn build_tool_calls_from_accumulated(
    tc_list: &[rho_ai::AccumulatedToolCall],
) -> Result<Vec<ModelToolCall>> {
    let mut tool_calls = Vec::new();
    for tc in tc_list {
        let id = tc.id.clone().ok_or_else(|| {
            RhoError::Agent(AgentError::ProtocolViolation(
                "tool call missing id".to_string(),
            ))
        })?;
        let name = tc.function_name.clone().ok_or_else(|| {
            RhoError::Agent(AgentError::ProtocolViolation(
                "tool call missing function name".to_string(),
            ))
        })?;
        tool_calls.push(ModelToolCall {
            id: ToolCallId::new(id),
            call_type: "function".to_owned(),
            function: ToolCallFunction {
                name: ToolName::new(name),
                arguments: tc.arguments.clone(),
            },
        });
    }
    Ok(tool_calls)
}

// ── Stream consumption ────────────────────────────────────────────────────────

/// Timeouts applied while consuming a model event stream.
///
/// Both fields are `Option`s: `None` disables that particular timeout and the
/// stream may block indefinitely. Use [`StreamTimeouts::none()`] to disable
/// both (e.g. for background LLM calls like compaction that don't loop through
/// the retry-aware `send_streaming` path).
///
/// # Why two timeouts
///
/// Reasoning models (DeepSeek-R1, Qwen3, o1-style) can legitimately spend a
/// long time before emitting the **first** token, but once streaming starts
/// the events arrive steadily. Separating the two lets you give the first
/// token a generous budget while still catching mid-stream stalls quickly.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StreamTimeouts {
    /// Maximum wait from the start of consumption until the first event.
    /// Guards against hung first-token requests.
    pub first_token: Option<std::time::Duration>,
    /// Maximum gap between consecutive events once streaming has started.
    /// Guards against a connection that stalls mid-response.
    pub idle: Option<std::time::Duration>,
}

impl StreamTimeouts {
    /// Disable both timeouts: the stream may block indefinitely.
    ///
    /// Equivalent to [`StreamTimeouts::default()`] but makes intent explicit
    /// at call sites.
    pub(crate) fn none() -> Self {
        Self {
            first_token: None,
            idle: None,
        }
    }
}

/// Translate a configured whole-seconds timeout into a `Duration`, treating
/// `0` as "disabled" (`None`).
fn secs_to_duration(secs: u64) -> Option<std::time::Duration> {
    (secs > 0).then_some(std::time::Duration::from_secs(secs))
}

/// Build a retryable [`RhoError`] for a stream stall.
fn stream_timeout_error(phase: &'static str, elapsed: std::time::Duration) -> RhoError {
    RhoError::Client(crate::client::error::ClientError::StreamTimeout {
        phase,
        elapsed_secs: elapsed.as_secs(),
    })
}

/// Consume an event stream, forwarding text/reasoning deltas to the observer
/// and collecting all events into a vector.
///
/// `timeouts` bounds how long the loop will wait:
/// - [`StreamTimeouts::first_token`] caps the wait for the first event.
/// - [`StreamTimeouts::idle`] caps the gap between consecutive events.
///
/// A stall beyond either bound returns a retryable
/// [`ClientError::StreamTimeout`](crate::client::error::ClientError::StreamTimeout)
/// so the caller (e.g. [`send_with_retry`](LoopContext::send_with_retry)) can
/// back off and try again instead of blocking forever.
///
/// Returns `Err` on the first stream error, converting the
/// [`ProviderError`](rho_ai::ProviderError) into a [`RhoError`].
pub(crate) async fn consume_stream(
    event_stream: rho_ai::EventStream,
    observer: &dyn AgentObserver,
    timeouts: StreamTimeouts,
) -> Result<Vec<rho_ai::StreamEvent>> {
    let mut events: Vec<rho_ai::StreamEvent> = Vec::new();
    let mut stream = std::pin::pin!(event_stream);
    let mut first = true;
    loop {
        // Pick the applicable limit *before* polling so the borrow taken by
        // `stream.next()` is released before the next iteration.
        let (limit, phase) = if first {
            first = false;
            (timeouts.first_token, "first token")
        } else {
            (timeouts.idle, "next chunk")
        };
        let item = match limit {
            Some(limit) => match tokio::time::timeout(limit, stream.next()).await {
                Ok(item) => item,
                Err(_) => return Err(stream_timeout_error(phase, limit)),
            },
            None => stream.next().await,
        };
        match item {
            None => break,
            Some(Ok(event)) => {
                match &event {
                    rho_ai::StreamEvent::Text(delta) => observer.on_text_delta(delta).await,
                    rho_ai::StreamEvent::Reasoning(delta) => {
                        observer.on_reasoning_delta(delta).await;
                    }
                    _ => {}
                }
                events.push(event);
            }
            Some(Err(e)) => {
                return Err(RhoError::Client(crate::client::error::ClientError::from(e)));
            }
        }
    }
    Ok(events)
}

// ── Response routing ───────────────────────────────────────────────────────────

/// Convert an accumulated LLM response into an [`AssistantResponse`] and
/// persist the assistant message to the session.
///
/// Handles three stop-reason paths:
/// - **`ToolUse`**: builds typed tool calls and persists them.
/// - **`Length`**: persists partial text and returns [`LengthTruncated`].
/// - **Other** (including `EndTurn`, `ContentFilter`): returns a text message,
///   but routes empty responses to [`LengthTruncated`] (llama.cpp workaround).
///
/// [`LengthTruncated`]: AssistantResponse::LengthTruncated
pub(crate) fn route_response(
    acc: &rho_ai::AccumulatedResponse,
    session: &mut Session,
) -> Result<AssistantResponse> {
    use crate::message::ContentBlock;

    // Accumulate token usage from this response.
    //
    // Cost enrichment: providers other than OpenRouter don't send a dollar
    // cost in the usage object. When the provider left `cost` unset, fall back
    // in order: (1) a user-defined `[[models]]` entry matching the model by
    // exact id, then (2) the built-in catalog via `Catalog::resolve` (basename
    // matching, tolerating native bare ids like `gpt-4o-2024-08-06`). User
    // entries win so unresolvable models (Groq `-instant`, self-hosted) still
    // accrue cost, and so users can override catalog pricing. Sentinels
    // (negatively-priced router models) yield `None`, which surfaces
    // downstream as "cost n/a" rather than a misleading $0.00.
    let mut usage = acc.usage.clone();
    if usage.cost.is_none() {
        if let Some(user_model) = session.user_models.iter().find(|m| m.id == session.model) {
            usage.cost = user_model.cost_for(&usage);
        } else if let Some(model) = rho_ai::Catalog::resolve(None, &session.model) {
            usage.cost = model.cost_for(&usage);
        }
    }
    session.accumulate_usage(&usage);

    let finish = crate::response::FinishReason::from(acc.stop_reason.clone());

    match &acc.stop_reason {
        rho_ai::StopReason::ToolUse => {
            let tool_calls = build_tool_calls_from_accumulated(&acc.tool_calls)?;
            session.append_assistant_message(crate::ChatMessage::Assistant {
                content: if acc.text.is_empty() {
                    vec![]
                } else {
                    vec![ContentBlock::Text {
                        text: acc.text.clone(),
                    }]
                },
                tool_calls: tool_calls.clone(),
                finish_reason: Some(finish.clone()),
            });
            Ok(AssistantResponse::ToolCalls(tool_calls))
        }
        rho_ai::StopReason::Length => {
            // Don't persist an empty assistant message. When the model returned
            // no content and no tool calls, the truncation handler will retry.
            // Persisting the empty message poisons the session tree — strict
            // models (OpenAI, Moonshot, Anthropic) reject empty assistant
            // messages with HTTP 400 on the next turn.
            if !acc.text.is_empty() {
                session.append_assistant_message(crate::ChatMessage::assistant_text_with_reason(
                    &acc.text,
                    finish.clone(),
                ));
            }
            Ok(AssistantResponse::LengthTruncated {
                content: acc.text.clone(),
                reasoning_content: acc.reasoning.clone(),
            })
        }
        _ => {
            let text = acc.text.clone();
            let reasoning = acc.reasoning.clone();

            // llama.cpp sometimes reports "stop" instead of "length"
            // when the model exhausts its completion budget and produces
            // nothing. Route to LengthTruncated so the agent loop can
            // attempt compaction and retry. ContentFilter is excluded
            // because retrying a filtered response is futile.
            if text.is_empty() && !matches!(acc.stop_reason, rho_ai::StopReason::ContentFilter) {
                // Don't persist the empty assistant message (see Length arm
                // above for rationale).
                return Ok(AssistantResponse::LengthTruncated {
                    content: text,
                    reasoning_content: reasoning,
                });
            }

            session.append_assistant_message(crate::ChatMessage::assistant_text_with_reason(
                &text,
                finish.clone(),
            ));
            Ok(AssistantResponse::Message {
                text: acc.text.clone(),
                reasoning_content: acc.reasoning.clone(),
            })
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::TokenBudget;
    use rho_ai::ToolDefinition;
    use std::sync::{Arc, Mutex};

    // ── Error type tests ──────────────────────────────────────────────────

    #[test]
    fn test_max_iterations_exceeded() {
        let error = AgentError::max_iterations_exceeded(100);
        assert!(matches!(error, AgentError::MaxIterationsExceeded(100)));
        assert_eq!(
            error.to_string(),
            "agent loop exceeded maximum iterations (100)"
        );
    }

    #[test]
    fn test_cancelled() {
        let error = AgentError::Cancelled;
        assert!(matches!(error, AgentError::Cancelled));
        assert_eq!(error.to_string(), "cancelled");
    }

    #[test]
    fn test_protocol_violation() {
        let error = AgentError::protocol_violation("invalid tool call format");
        assert!(matches!(error, AgentError::ProtocolViolation(_)));
        assert_eq!(
            error.to_string(),
            "protocol violation: invalid tool call format"
        );
    }

    #[test]
    fn test_agent_result_type_ok() {
        let result: AgentResultType<String> = Ok("success".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_result_type_err() {
        let result: AgentResultType<String> = Err(AgentError::Cancelled);
        assert!(result.is_err());
    }

    #[test]
    fn test_display_formats() {
        assert_eq!(
            AgentError::MaxIterationsExceeded(50).to_string(),
            "agent loop exceeded maximum iterations (50)"
        );
        assert_eq!(AgentError::Cancelled.to_string(), "cancelled");
        assert_eq!(
            AgentError::ProtocolViolation("test".to_string()).to_string(),
            "protocol violation: test"
        );
    }

    #[test]
    fn test_error_debug() {
        let error = AgentError::ProtocolViolation("debug test".to_string());
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("ProtocolViolation"));
        assert!(debug_str.contains("debug test"));
    }

    #[test]
    fn intercept_result_allow_is_not_block() {
        let result = InterceptResult::Allow;
        assert!(
            !matches!(result, InterceptResult::Block { .. }),
            "Allow should not match Block"
        );
    }

    #[test]
    fn intercept_result_block_carries_reason() {
        let result = InterceptResult::Block {
            reason: "force flags not allowed".to_string(),
        };
        if let InterceptResult::Block { reason } = result {
            assert_eq!(reason, "force flags not allowed");
        } else {
            panic!("expected Block");
        }
    }

    /// An observer that blocks any tool call whose arguments contain "--force".
    struct ForceBlockObserver;

    impl AgentObserver for ForceBlockObserver {
        fn on_tool_call_intercept(&self, _name: &str, arguments: &str) -> Option<InterceptResult> {
            if arguments.contains("--force") || arguments.contains("-Force") {
                return Some(InterceptResult::Block {
                    reason: "force flags blocked by extension".to_string(),
                });
            }
            None
        }
    }

    #[test]
    fn force_block_observer_blocks_force_flags() {
        let obs = ForceBlockObserver;
        let result = obs.on_tool_call_intercept("run_command", "git push --force");
        assert!(
            matches!(result, Some(InterceptResult::Block { .. })),
            "should block --force"
        );
    }

    #[test]
    fn force_block_observer_allows_normal_calls() {
        let obs = ForceBlockObserver;
        let result = obs.on_tool_call_intercept("run_command", "git push");
        assert!(result.is_none(), "should allow normal calls");
    }

    // ── build_llm_request tests ──────────────────────────────────────────

    fn test_session(
        system: Option<&str>,
        user_msgs: &[&str],
        tools: Vec<ToolDefinition>,
    ) -> Session {
        let mut session = Session::in_memory("test-model", system, tools, "/tmp");
        for msg in user_msgs {
            session.append_user_message(msg);
        }
        session
    }

    #[test]
    fn build_llm_request_includes_model() {
        let session = test_session(None, &["hello"], vec![]);
        let req = build_llm_request(&session);
        assert_eq!(req.model, "test-model");
    }

    #[test]
    fn build_llm_request_includes_messages() {
        let session = test_session(Some("Be helpful."), &["hello"], vec![]);
        let req = build_llm_request(&session);
        // System + User = 2 messages.
        assert_eq!(req.messages.len(), 2);
        assert!(matches!(&req.messages[0], rho_ai::LlmMessage::System(t) if t == "Be helpful."));
        assert!(matches!(&req.messages[1], rho_ai::LlmMessage::User(t) if t == "hello"));
    }

    #[test]
    fn build_llm_request_includes_tools() {
        let tools = vec![ToolDefinition::new(
            "read_file",
            "Read a file",
            serde_json::json!({"type": "object"}),
        )];
        let session = test_session(None, &[], tools);
        let req = build_llm_request(&session);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "read_file");
    }

    #[test]
    fn build_llm_request_includes_max_tokens() {
        let session = test_session(None, &["hello"], vec![])
            .with_token_budget(TokenBudget::with_reserve(4096, 2048));
        let req = build_llm_request(&session);
        assert_eq!(req.max_tokens, Some(2048));
    }

    // ── consume_stream tests ─────────────────────────────────────────────

    /// An observer that records all text and reasoning deltas.
    #[derive(Default)]
    struct RecordingObserver {
        text_deltas: Arc<Mutex<Vec<String>>>,
        reasoning_deltas: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AgentObserver for RecordingObserver {
        async fn on_text_delta(&self, delta: &str) {
            self.text_deltas.lock().unwrap().push(delta.to_owned());
        }
        async fn on_reasoning_delta(&self, delta: &str) {
            self.reasoning_deltas.lock().unwrap().push(delta.to_owned());
        }
    }

    #[tokio::test]
    async fn consume_stream_collects_events() {
        let events = vec![
            Ok(rho_ai::StreamEvent::Text("hello".into())),
            Ok(rho_ai::StreamEvent::Text(" world".into())),
            Ok(rho_ai::StreamEvent::Done {
                reason: rho_ai::StopReason::EndTurn,
                usage: rho_ai::StreamUsage::default(),
            }),
        ];
        let stream: rho_ai::EventStream = Box::pin(futures::stream::iter(events));
        let observer = RecordingObserver::default();
        let collected = consume_stream(stream, &observer, StreamTimeouts::none())
            .await
            .unwrap();
        assert_eq!(collected.len(), 3);
        assert_eq!(
            *observer.text_deltas.lock().unwrap(),
            vec!["hello", " world"]
        );
    }

    #[tokio::test]
    async fn consume_stream_forwards_reasoning_deltas() {
        let events = vec![
            Ok(rho_ai::StreamEvent::Reasoning("thinking".into())),
            Ok(rho_ai::StreamEvent::Done {
                reason: rho_ai::StopReason::EndTurn,
                usage: rho_ai::StreamUsage::default(),
            }),
        ];
        let stream: rho_ai::EventStream = Box::pin(futures::stream::iter(events));
        let observer = RecordingObserver::default();
        let _ = consume_stream(stream, &observer, StreamTimeouts::none())
            .await
            .unwrap();
        assert_eq!(*observer.reasoning_deltas.lock().unwrap(), vec!["thinking"]);
    }

    #[tokio::test]
    async fn consume_stream_propagates_error() {
        let events: Vec<std::result::Result<rho_ai::StreamEvent, rho_ai::ProviderError>> =
            vec![Err(rho_ai::ProviderError::Sse {
                message: "boom".into(),
            })];
        let stream: rho_ai::EventStream = Box::pin(futures::stream::iter(events));
        let observer = RecordingObserver::default();
        let result = consume_stream(stream, &observer, StreamTimeouts::none()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn consume_stream_first_token_timeout_is_retryable_error() {
        // A stream that never yields any events simulates a hung first-token
        // request (the exact failure mode observed in the wild).
        let stream: rho_ai::EventStream = Box::pin(futures::stream::pending::<
            std::result::Result<rho_ai::StreamEvent, rho_ai::ProviderError>,
        >());
        let observer = RecordingObserver::default();
        let timeouts = StreamTimeouts {
            first_token: Some(std::time::Duration::from_millis(50)),
            idle: None,
        };
        let result = consume_stream(stream, &observer, timeouts).await;
        let err = result.expect_err("should time out");
        assert!(err.is_retryable(), "stream timeout must be retryable");
        assert!(matches!(
            err,
            RhoError::Client(crate::client::error::ClientError::StreamTimeout {
                phase: "first token",
                elapsed_secs: 0,
            })
        ));
    }

    #[tokio::test]
    async fn consume_stream_idle_timeout_fires_after_first_event() {
        // One event arrives, then the stream stalls forever. The idle timeout
        // (not the first-token timeout) should fire.
        let first = Ok(rho_ai::StreamEvent::Text("hello".into()));
        let stalled = futures::stream::pending::<
            std::result::Result<rho_ai::StreamEvent, rho_ai::ProviderError>,
        >();
        let stream: rho_ai::EventStream = Box::pin(futures::stream::iter([first]).chain(stalled));
        let observer = RecordingObserver::default();
        let timeouts = StreamTimeouts {
            first_token: None,
            idle: Some(std::time::Duration::from_millis(50)),
        };
        let result = consume_stream(stream, &observer, timeouts).await;
        let err = result.expect_err("should time out after the first event");
        assert!(err.is_retryable());
        assert!(matches!(
            err,
            RhoError::Client(crate::client::error::ClientError::StreamTimeout {
                phase: "next chunk",
                elapsed_secs: 0,
            })
        ));
        // The first event was still delivered to the observer.
        assert_eq!(
            *observer.text_deltas.lock().unwrap(),
            vec!["hello".to_owned()]
        );
    }

    #[tokio::test]
    async fn consume_stream_disabled_timeouts_block_until_completion() {
        // With both timeouts disabled, a finite stream completes normally.
        let events = vec![
            Ok(rho_ai::StreamEvent::Text("hi".into())),
            Ok(rho_ai::StreamEvent::Done {
                reason: rho_ai::StopReason::EndTurn,
                usage: rho_ai::StreamUsage::default(),
            }),
        ];
        let stream: rho_ai::EventStream = Box::pin(futures::stream::iter(events));
        let observer = RecordingObserver::default();
        let collected = consume_stream(stream, &observer, StreamTimeouts::none())
            .await
            .unwrap();
        assert_eq!(collected.len(), 2);
    }

    // ── route_response tests ─────────────────────────────────────────────

    #[test]
    fn route_response_text_message() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "hello".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        match response {
            AssistantResponse::Message {
                text,
                reasoning_content,
            } => {
                assert_eq!(text, "hello");
                assert!(reasoning_content.is_empty());
            }
            _ => panic!("expected Message, got {response:?}"),
        }
    }

    #[test]
    fn route_response_text_with_reasoning() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "done".into(),
            reasoning: "I thought about it".into(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        match response {
            AssistantResponse::Message {
                text,
                reasoning_content,
            } => {
                assert_eq!(text, "done");
                assert_eq!(reasoning_content, "I thought about it");
            }
            _ => panic!("expected Message, got {response:?}"),
        }
    }

    #[test]
    fn route_response_tool_calls() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![rho_ai::AccumulatedToolCall {
                id: Some("call_1".into()),
                function_name: Some("read_file".into()),
                arguments: r#"{\"path\":\"a.rs\"}"#.into(),
            }],
            stop_reason: rho_ai::StopReason::ToolUse,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        match response {
            AssistantResponse::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id.to_string(), "call_1");
                assert_eq!(calls[0].function.name.to_string(), "read_file");
            }
            _ => panic!("expected ToolCalls, got {response:?}"),
        }
    }

    #[test]
    fn route_response_length_truncated() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "partial".into(),
            reasoning: "thinking".into(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::Length,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        match response {
            AssistantResponse::LengthTruncated {
                content,
                reasoning_content,
            } => {
                assert_eq!(content, "partial");
                assert_eq!(reasoning_content, "thinking");
            }
            _ => panic!("expected LengthTruncated, got {response:?}"),
        }
    }

    #[test]
    fn route_response_empty_end_turn_routes_to_length_truncated() {
        // llama.cpp workaround: empty response with stop reason should be
        // treated as length truncation.
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        assert!(
            matches!(response, AssistantResponse::LengthTruncated { .. }),
            "empty EndTurn should route to LengthTruncated, got {response:?}"
        );
    }

    #[test]
    fn route_response_empty_content_filter_stays_as_message() {
        // ContentFilter with empty text should NOT be treated as truncation.
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::ContentFilter,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        assert!(
            matches!(response, AssistantResponse::Message { .. }),
            "empty ContentFilter should stay as Message, got {response:?}"
        );
    }

    #[test]
    fn route_response_persists_assistant_message_for_text() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "reply".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        // The session should have an assistant message persisted.
        let msgs = session.path_messages();
        let last = msgs.last().expect("should have a message");
        assert!(
            matches!(last, ChatMessage::Assistant { .. }),
            "expected Assistant message, got {last:?}"
        );
    }

    #[test]
    fn route_response_persists_finish_reason_for_end_turn() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "reply".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        let msgs = session.path_messages();
        let last = msgs.last().expect("should have a message");
        match last {
            ChatMessage::Assistant { finish_reason, .. } => {
                assert_eq!(*finish_reason, Some(crate::response::FinishReason::Stop));
            }
            other => panic!("expected Assistant message, got {other:?}"),
        }
    }

    #[test]
    fn route_response_persists_finish_reason_for_tool_use() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![rho_ai::AccumulatedToolCall {
                id: Some("c1".into()),
                function_name: Some("read_file".into()),
                arguments: "{}".into(),
            }],
            stop_reason: rho_ai::StopReason::ToolUse,
            usage: rho_ai::StreamUsage::default(),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        let msgs = session.path_messages();
        let last = msgs.last().expect("should have a message");
        match last {
            ChatMessage::Assistant { finish_reason, .. } => {
                assert_eq!(
                    *finish_reason,
                    Some(crate::response::FinishReason::ToolCalls)
                );
            }
            other => panic!("expected Assistant message, got {other:?}"),
        }
    }

    #[test]
    fn route_response_persists_finish_reason_for_length() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: "partial".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::Length,
            usage: rho_ai::StreamUsage::default(),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        let msgs = session.path_messages();
        let last = msgs.last().expect("should have a message");
        match last {
            ChatMessage::Assistant { finish_reason, .. } => {
                assert_eq!(*finish_reason, Some(crate::response::FinishReason::Length));
            }
            other => panic!("expected Assistant message, got {other:?}"),
        }
    }

    #[test]
    fn route_response_persists_finish_reason_for_empty_stop() {
        // The exact failure mode from the field: an empty reply that closed
        // successfully. The empty assistant message must NOT be persisted
        // — strict-validation models (OpenAI, Moonshot) reject it with HTTP 400.
        // It still routes to LengthTruncated so the agent loop can retry.
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        assert!(
            matches!(response, AssistantResponse::LengthTruncated { .. }),
            "empty EndTurn should route to LengthTruncated, got {response:?}"
        );
        // The session should NOT contain the empty assistant message.
        let msgs = session.path_messages();
        let assistant_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .collect();
        assert!(
            assistant_msgs.is_empty(),
            "empty EndTurn response should not be persisted, but found {} assistant message(s)",
            assistant_msgs.len()
        );
    }

    #[test]
    fn record_turn_error_appends_diagnostic_marker() {
        let mut session = test_session(None, &["hi"], vec![]);
        let error = RhoError::Client(crate::client::error::ClientError::StreamTimeout {
            phase: "first token",
            elapsed_secs: 90,
        });
        record_turn_error(&mut session, &error);
        let leaf = session.leaf().expect("leaf should point at the marker");
        let entry = session.entry(&leaf).expect("marker entry should exist");
        match &entry.payload {
            crate::session::EntryPayload::Custom { kind, data } => {
                assert_eq!(kind, "rho.turn_error.v1");
                assert_eq!(data["category"], "client");
                assert!(
                    data["error"]
                        .as_str()
                        .is_some_and(|s| s.contains("first token")),
                    "error message should mention the phase"
                );
            }
            other => panic!("expected Custom marker, got {other:?}"),
        }
    }

    #[test]
    fn record_turn_error_skips_cancellation() {
        let mut session = test_session(None, &["hi"], vec![]);
        let leaf_before = session.leaf();
        record_turn_error(&mut session, &RhoError::Agent(AgentError::Cancelled));
        assert_eq!(
            session.leaf(),
            leaf_before,
            "cancellation is user-initiated and must not append a marker"
        );
    }

    #[test]
    fn route_response_persists_tool_calls() {
        let mut session = test_session(None, &[], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![rho_ai::AccumulatedToolCall {
                id: Some("c1".into()),
                function_name: Some("edit_file".into()),
                arguments: "{}".into(),
            }],
            stop_reason: rho_ai::StopReason::ToolUse,
            usage: rho_ai::StreamUsage::default(),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        let msgs = session.path_messages();
        let last = msgs.last().expect("should have a message");
        match last {
            ChatMessage::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
            }
            _ => panic!("expected Assistant with tool_calls, got {last:?}"),
        }
    }

    #[test]
    fn route_response_enriches_cost_from_catalog_when_provider_omits_it() {
        // Provider did not report a cost (usage.cost is None, as for any
        // non-OpenRouter provider). With a catalog model set, route_response
        // should fall back to per-million pricing.
        let mut session = test_session(None, &[], vec![]);
        session.model = "z-ai/glm-5.2".to_string();
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000_000, 0),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        // glm-5.2 input is $0.826/M → 1M tokens should cost $0.826.
        let cost = session.api_usage().total_cost;
        assert!(
            (cost - 0.826).abs() < 1e-9,
            "expected catalog-derived cost, got {cost}"
        );
    }

    #[test]
    fn route_response_leaves_cost_unset_when_provider_reports_it() {
        // When the provider DOES report cost, the catalog fallback must not
        // override it.
        let mut session = test_session(None, &[], vec![]);
        session.model = "z-ai/glm-5.2".to_string();
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000_000, 0).with_cost(0.42),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        assert!((session.api_usage().total_cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn route_response_leaves_cost_unset_for_unknown_model() {
        // A model not in the catalog (and no provider cost) stays unpriced,
        // so the frontend can surface "cost n/a" rather than $0.
        let mut session = test_session(None, &[], vec![]); // model = "test-model"
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000, 500),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        assert!(session.api_usage().total_cost.abs() < f64::EPSILON);
        assert_eq!(session.api_usage().request_count, 1);
    }

    #[test]
    fn route_response_enriches_cost_for_native_bare_id() {
        // A native provider (OpenAI direct) sends a bare, undated-ish model id
        // with no `provider/` prefix and no dollar cost. Catalog basename
        // resolution should still price it so the session accrues cost.
        let mut session = test_session(None, &[], vec![]);
        session.model = "gpt-4o-2024-08-06".to_string();
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000_000, 0),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        assert!(
            session.api_usage().total_cost > 0.0,
            "bare native id should resolve via catalog basename; got {}",
            session.api_usage().total_cost,
        );
    }

    #[test]
    fn route_response_enriches_cost_from_user_defined_model() {
        // A model the catalog can't resolve (invented id) is priced via a
        // user-defined [[models]] entry instead of surfacing "cost n/a".
        let mut session = test_session(None, &[], vec![]);
        session.model = "groq/llama-3.1-8b-instant".to_string();
        session.user_models = vec![rho_ai::Model {
            id: "groq/llama-3.1-8b-instant".to_string(),
            name: "groq/llama-3.1-8b-instant".to_string(),
            provider: "groq".to_string(),
            context_window: 0,
            max_tokens: 0,
            input: rho_ai::ModelInput::default(),
            cost: rho_ai::ModelCost {
                input: 0.05,
                output: 0.08,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            thinking: rho_ai::ModelThinking::default(),
        }];
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000_000, 0),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        // 1M input @ $0.05/M = $0.05
        assert!(
            (session.api_usage().total_cost - 0.05).abs() < 1e-9,
            "user-defined model should price the turn; got {}",
            session.api_usage().total_cost,
        );
    }

    #[test]
    fn route_response_user_model_overrides_catalog() {
        // When a user-defined entry shares the id with a catalog entry, the
        // user's pricing wins (consulted first).
        let mut session = test_session(None, &[], vec![]);
        session.model = "z-ai/glm-5.2".to_string(); // catalog input is 1.4
        session.user_models = vec![rho_ai::Model {
            id: "z-ai/glm-5.2".to_string(),
            name: "z-ai/glm-5.2".to_string(),
            provider: "z-ai".to_string(),
            context_window: 0,
            max_tokens: 0,
            input: rho_ai::ModelInput::default(),
            cost: rho_ai::ModelCost {
                input: 9.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            thinking: rho_ai::ModelThinking::default(),
        }];
        let acc = rho_ai::AccumulatedResponse {
            text: "hi".into(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::new(1_000_000, 0),
        };
        let _ = route_response(&acc, &mut session).unwrap();
        // User's $9.0/M input wins, not the catalog's $0.826.
        assert!(
            (session.api_usage().total_cost - 9.0).abs() < 1e-9,
            "user model pricing should override the catalog; got {}",
            session.api_usage().total_cost,
        );
    }

    #[test]
    fn route_response_empty_length_does_not_persist() {
        // When the model returns finish_reason=length with empty content and no
        // tool calls, the empty assistant message must NOT be persisted to the
        // session. Persisting it poisons the session tree — strict-validation
        // models (OpenAI, Moonshot, Anthropic) reject empty assistant messages
        // with HTTP 400 on the next turn.
        let mut session = test_session(None, &["hello"], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::Length,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        // It should still return LengthTruncated so the agent loop can retry.
        assert!(
            matches!(response, AssistantResponse::LengthTruncated { .. }),
            "empty Length should return LengthTruncated, got {response:?}"
        );
        // But the session should NOT contain the empty assistant message.
        let msgs = session.path_messages();
        let assistant_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .collect();
        assert!(
            assistant_msgs.is_empty(),
            "empty Length response should not be persisted, but found {} assistant message(s)",
            assistant_msgs.len()
        );
    }

    #[test]
    fn route_response_empty_stop_does_not_persist() {
        // Same bug, different path: empty EndTurn (llama.cpp workaround) should
        // not persist an empty assistant message either.
        let mut session = test_session(None, &["hello"], vec![]);
        let acc = rho_ai::AccumulatedResponse {
            text: String::new(),
            reasoning: String::new(),
            tool_calls: vec![],
            stop_reason: rho_ai::StopReason::EndTurn,
            usage: rho_ai::StreamUsage::default(),
        };
        let response = route_response(&acc, &mut session).unwrap();
        assert!(
            matches!(response, AssistantResponse::LengthTruncated { .. }),
            "empty EndTurn should route to LengthTruncated, got {response:?}"
        );
        let msgs = session.path_messages();
        let assistant_msgs: Vec<_> = msgs
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .collect();
        assert!(
            assistant_msgs.is_empty(),
            "empty EndTurn response should not be persisted, but found {} assistant message(s)",
            assistant_msgs.len()
        );
    }

    // ── build_tool_calls_from_accumulated tests ──────────────────────────

    #[test]
    fn build_tool_calls_from_accumulated_success() {
        let acc = vec![rho_ai::AccumulatedToolCall {
            id: Some("call_abc".into()),
            function_name: Some("read_file".into()),
            arguments: r#"{\"path\":\"x.rs\"}"#.into(),
        }];
        let calls = build_tool_calls_from_accumulated(&acc).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.to_string(), "call_abc");
        assert_eq!(calls[0].function.name.to_string(), "read_file");
    }

    #[test]
    fn build_tool_calls_from_accumulated_missing_id() {
        let acc = vec![rho_ai::AccumulatedToolCall {
            id: None,
            function_name: Some("read_file".into()),
            arguments: String::new(),
        }];
        let result = build_tool_calls_from_accumulated(&acc);
        assert!(result.is_err());
    }

    #[test]
    fn build_tool_calls_from_accumulated_missing_name() {
        let acc = vec![rho_ai::AccumulatedToolCall {
            id: Some("call_1".into()),
            function_name: None,
            arguments: String::new(),
        }];
        let result = build_tool_calls_from_accumulated(&acc);
        assert!(result.is_err());
    }

    #[test]
    fn build_tool_calls_from_accumulated_empty() {
        let calls = build_tool_calls_from_accumulated(&[]).unwrap();
        assert!(calls.is_empty());
    }
}
