//! Agent loop state machine.
//!
//! [`run_loop`] drives the conversation until the model stops or a budget is
//! exhausted. Internally, the loop is a state machine where each [`State`]
//! transition is handled by a method on [`LoopContext`]. Every transition is
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

use thiserror::Error;

use crate::approval::{ApprovalGate, ApprovalPolicy, DefaultApprovalPolicy};
use crate::conversation::AssistantResponse;
use crate::error::{Result, RhoError};
use crate::message::ChatMessage;
use crate::message::{ModelToolCall, ToolCallFunction};
use crate::newtypes::{ToolCallId, ToolName};
use crate::session::Session;
use crate::tool::{CancellationToken, Tool, ToolRegistry, ToolResult, ToolRisk};
use futures::StreamExt;
use std::collections::HashMap;
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
pub type AgentResult<T> = std::result::Result<T, AgentError>;

impl From<AgentError> for crate::error::RhoError {
    fn from(error: AgentError) -> Self {
        crate::error::RhoError::Agent(error)
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
pub trait AgentObserver: Send + Sync {
    /// The agent entered a new [`AgentState`].
    fn on_state_change(&self, _state: AgentState) {}

    /// Incremental text content from the model's streaming response.
    ///
    /// May be called many times per loop iteration as deltas arrive.
    fn on_text_delta(&self, _delta: &str) {}

    /// Incremental reasoning / chain-of-thought content from the model.
    ///
    /// May be called many times per loop iteration as deltas arrive.
    fn on_reasoning_delta(&self, _delta: &str) {}

    /// The model requested a tool call with the given name and arguments.
    fn on_tool_call(&self, _name: &str, _arguments: &str) {}

    /// A tool finished executing and produced this result.
    fn on_tool_result(&self, _name: &str, _result: &ToolResult) {}

    /// A tool call was denied by the approval gate.
    fn on_tool_denied(&self, _name: &str) {}

    /// A tool call requires human approval with the given risk level.
    fn on_approval_requested(&self, _tool_name: &str, _risk: ToolRisk) {}
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
    /// Whether to display full chain-of-thought reasoning in the output.
    /// When `false`, shows a one-line summary instead.
    pub show_reasoning: bool,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("max_iterations", &self.max_iterations)
            .field("retry_budget", &self.retry_budget)
            .field("initial_backoff_ms", &self.initial_backoff_ms)
            .field("approval_policy", &"<dyn ApprovalPolicy>")
            .field("stuck_loop_threshold", &self.stuck_loop_threshold)
            .field("show_reasoning", &self.show_reasoning)
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
            show_reasoning: false,
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
            show_reasoning: config.agent.show_reasoning,
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
/// };
/// let reply = run_loop(&mut session, "hello", &params).await?;
/// ```
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

    /// Terminal state — the agent produced a final text reply.
    Done(String),
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
}

impl LoopContext<'_> {
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
            State::Done(_) => unreachable!("Done is terminal"),
        }
    }

    // ── Thinking ─────────────────────────────────────────────────────────

    /// Send the conversation to the LLM and route the response.
    async fn handle_thinking(&mut self) -> Result<State> {
        if self.params.cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }

        let response = self.send_with_retry().await?;
        self.iterations += 1;

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
                info!(reply_len = text.len());
                if !reasoning_content.is_empty() {
                    info!(
                        reasoning_len = reasoning_content.len(),
                        "model returned reasoning content with stop finish_reason"
                    );
                }
                self.params.observer.on_state_change(AgentState::Idle);
                let reply = self.format_reply(text, &reasoning_content);
                Ok(State::Done(reply))
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
                Ok(self.classify_first_call(calls))
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

        let approved = self.params.gate.request_approval(&call, risk).await;

        if !approved {
            debug!(tool_name = %call.function.name, action = "denied");
            self.params.observer.on_tool_denied(&call.function.name);
            let call_id = ToolCallId::new(call.id.to_string());
            let _ = self
                .session
                .append_tool_result(call_id, &ToolResult::error("Tool call denied by user."));
            return Ok(self.advance_to_next_call(remaining));
        }

        // Approved — transition to executing.
        self.params
            .observer
            .on_state_change(AgentState::ExecutingTool);
        Ok(State::ExecutingTool { call, remaining })
    }

    // ── ExecutingTool ────────────────────────────────────────────────────

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
                let _ = self
                    .session
                    .append_tool_result(call_id, &ToolResult::error(format!("{e}")));
                return Ok(self.advance_to_next_call(remaining));
            }
        };

        // Stuck-loop detection.
        if self.params.config.stuck_loop_threshold > 0
            && let Some(nudge) = self.check_stuck_loop(&call, &result)
        {
            let _ = self.session.append_tool_result(call_id, &nudge);
            return Ok(self.advance_to_next_call(remaining));
        }

        self.params
            .observer
            .on_tool_result(&call.function.name, &result);
        let _ = self.session.append_tool_result(call_id, &result);
        Ok(self.advance_to_next_call(remaining))
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
            warn!("model produced empty response, injecting nudge and retrying");
            self.session.append_user_message(
                "Your previous response was empty. Please provide a \
                 tool call or text response and try again.",
            );
            return Ok(State::Thinking);
        }

        // Attempt compaction to free context space, then retry.
        let strategy = crate::session::MechanicalCompactionStrategy::new();
        let budget = self.session.message_budget();
        let compact_threshold = budget / 4;

        match self
            .session
            .compact_older_than(compact_threshold, &strategy)
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
                self.params.observer.on_state_change(AgentState::Idle);
                Ok(State::Done(explanation))
            }
        }
    }

    // ── Call classification helpers ──────────────────────────────────────

    /// From a batch of tool calls, classify the first one (approval or
    /// execute) and stash the rest.
    fn classify_first_call(&mut self, calls: Vec<ModelToolCall>) -> State {
        let mut iter = calls.into_iter();
        // SAFETY: callers check `calls.is_empty()` before calling this.
        let first = iter.next().expect("calls is non-empty");
        let remaining: Vec<_> = iter.collect();
        self.classify_call(first, remaining)
    }

    /// After processing one tool call, determine the state for the next one
    /// (or go back to [`State::Thinking`] if none remain).
    fn advance_to_next_call(&mut self, remaining: Vec<ModelToolCall>) -> State {
        let mut iter = remaining.into_iter();
        if let Some(next) = iter.next() {
            self.classify_call(next, iter.collect())
        } else {
            self.params.observer.on_state_change(AgentState::Thinking);
            State::Thinking
        }
    }

    /// Classify a single tool call: does it need approval, or can it be
    /// executed directly?
    fn classify_call(&mut self, call: ModelToolCall, remaining: Vec<ModelToolCall>) -> State {
        let risk = self
            .params
            .registry
            .get_by_name(&call.function.name)
            .map_or(ToolRisk::Destructive, Tool::risk);

        self.params
            .observer
            .on_tool_call(&call.function.name, &call.function.arguments);

        if self
            .params
            .config
            .approval_policy
            .requires_approval(&call.function.name, risk)
        {
            self.params
                .observer
                .on_state_change(AgentState::AwaitingApproval);
            self.params
                .observer
                .on_approval_requested(&call.function.name, risk);
            State::AwaitingApproval { call, remaining }
        } else {
            self.params
                .observer
                .on_state_change(AgentState::ExecutingTool);
            State::ExecutingTool { call, remaining }
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
            Some(ToolResult::error(format!(
                "STUCK LOOP DETECTED: you have called `{}` with the \
                 same arguments {count} times and received the same result \
                 each time. The file on disk has NOT changed between \
                 calls. You must use edit_file or write_file to modify \
                 the source code BEFORE running the command again. \
                 Re-read the file with read_file to see its current \
                 state, then apply the necessary edits.",
                call.function.name,
            )))
        } else {
            None
        }
    }

    // ── Formatting helpers ───────────────────────────────────────────────

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

        let events = consume_stream(event_stream, self.params.observer).await?;
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
/// - [`RhoError::MaxIterationsExceeded`] — loop ran past `config.max_iterations`
/// - [`RhoError::RetryBudgetExhausted`] — transient error retried too many times
/// - Any fatal error from the client or tool registry
#[tracing::instrument(skip_all, fields(input_len = message.len()))]
pub async fn run_loop(
    session: &mut Session,
    message: &str,
    params: &LoopParams<'_>,
) -> Result<String> {
    info!("run_loop called with message: {}", message);
    session.append_user_message(message);

    let mut ctx = LoopContext {
        session,
        params,
        repetition_counts: HashMap::new(),
        iterations: 0,
    };

    let mut state = State::Thinking;
    params.observer.on_state_change(AgentState::Thinking);

    loop {
        let _iter_span =
            tracing::info_span!("agent_iteration", iteration = ctx.iterations).entered();
        state = ctx.step(state).await?;
        if let State::Done(text) = state {
            return Ok(text);
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Build an [`LlmRequest`] from the current session state.
///
/// Converts session messages to [`LlmMessage`] via [`ChatMessage::to_llm_message`],
/// maps tool schemas to [`ToolDefinition`](rho_ai::ToolDefinition), and sets
/// `max_tokens` from the session's token budget.
fn build_llm_request(session: &Session) -> rho_ai::LlmRequest {
    use rho_ai::ToolDefinition;

    let fitted = session.path_messages();
    let llm_messages: Vec<rho_ai::LlmMessage> =
        fitted.iter().map(ChatMessage::to_llm_message).collect();

    let tools: Vec<ToolDefinition> = session
        .tools
        .iter()
        .map(|t| {
            ToolDefinition::new(
                t.function.name.clone(),
                t.function.description.clone(),
                t.function.parameters.clone(),
            )
        })
        .collect();

    rho_ai::LlmRequest {
        model: session.model.clone(),
        messages: llm_messages,
        tools,
        max_tokens: Some(session.token_budget().completion_reserve),
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

/// Consume an event stream, forwarding text/reasoning deltas to the observer
/// and collecting all events into a vector.
///
/// Returns `Err` on the first stream error, converting the
/// [`ProviderError`](rho_ai::ProviderError) into a [`RhoError`].
async fn consume_stream(
    event_stream: rho_ai::EventStream,
    observer: &dyn AgentObserver,
) -> Result<Vec<rho_ai::StreamEvent>> {
    let mut events: Vec<rho_ai::StreamEvent> = Vec::new();
    let mut stream = std::pin::pin!(event_stream);
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                match &event {
                    rho_ai::StreamEvent::Text(delta) => observer.on_text_delta(delta),
                    rho_ai::StreamEvent::Reasoning(delta) => {
                        observer.on_reasoning_delta(delta);
                    }
                    _ => {}
                }
                events.push(event);
            }
            Err(e) => {
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
fn route_response(
    acc: &rho_ai::AccumulatedResponse,
    session: &mut Session,
) -> Result<AssistantResponse> {
    use crate::message::ContentBlock;

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
            });
            Ok(AssistantResponse::ToolCalls(tool_calls))
        }
        rho_ai::StopReason::Length => {
            session.append_assistant_message(crate::ChatMessage::assistant_text(&acc.text));
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
                session.append_assistant_message(crate::ChatMessage::assistant_text(&text));
                return Ok(AssistantResponse::LengthTruncated {
                    content: text,
                    reasoning_content: reasoning,
                });
            }

            session.append_assistant_message(crate::ChatMessage::assistant_text(&text));
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
    use crate::schema::ToolSchema;
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
    fn test_agent_result_ok() {
        let result: AgentResult<String> = Ok("success".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_result_err() {
        let result: AgentResult<String> = Err(AgentError::Cancelled);
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

    // ── build_llm_request tests ──────────────────────────────────────────

    fn test_session(system: Option<&str>, user_msgs: &[&str], tools: Vec<ToolSchema>) -> Session {
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
        let tools = vec![ToolSchema::function(
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

    impl AgentObserver for RecordingObserver {
        fn on_text_delta(&self, delta: &str) {
            self.text_deltas.lock().unwrap().push(delta.to_owned());
        }
        fn on_reasoning_delta(&self, delta: &str) {
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
        let collected = consume_stream(stream, &observer).await.unwrap();
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
        let _ = consume_stream(stream, &observer).await.unwrap();
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
        let result = consume_stream(stream, &observer).await;
        assert!(result.is_err());
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
