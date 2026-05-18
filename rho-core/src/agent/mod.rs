//! Agent loop state machine.
//!
//! [`run_loop`] drives the conversation until the model stops or a budget is
//! exhausted. [`AgentState`] is the observable state for UIs and tests.
//! [`TransitionError`] classifies errors as retryable or fatal.

pub mod error;

use crate::agent::error::AgentError;
use crate::approval::{ApprovalGate, ApprovalPolicy, DefaultApprovalPolicy};
use crate::client::ChatClient;
use crate::conversation::AssistantResponse;
use crate::error::{Result, RhoError};
use crate::message::{ModelToolCall, ToolCallFunction};
use crate::newtypes::{ToolCallId, ToolName};
use crate::request::ChatRequest;
use crate::response::FinishReason;
use crate::session::Session;
use crate::stream::AccumulatedToolCall;
use crate::stream::StreamChunk;
use crate::tool::{CancellationToken, Tool, ToolRegistry, ToolResult};
use futures::StreamExt;
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
            show_reasoning: config.agent.show_reasoning,
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
    info!("run_loop called with message: {}", message);
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
            return Err(AgentError::Cancelled.into());
        }

        let response = send_with_retry_streaming(session, client, config).await?;

        iterations += 1;
        if iterations > config.max_iterations {
            error!(max = config.max_iterations);
            return Err(AgentError::MaxIterationsExceeded(config.max_iterations).into());
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
                } else if config.show_reasoning {
                    format!("<thinking>\n{reasoning_content}\n</thinking>\n\n{text}")
                } else {
                    let reasoning_tokens = reasoning_content.len() / 4;
                    format!("[reasoning: ~{reasoning_tokens} tokens]\n\n{text}")
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

                // When both content and reasoning are empty, the model
                // produced nothing at all. This is common with reasoning
                // models (llama.cpp, LM Studio) that report "stop" instead
                // of "length" when the completion budget is exhausted during
                // thinking. Rather than attempting compaction (which will
                // fail with too few entries), inject a nudge so the model
                // can see the empty response and try again.
                if content.is_empty() && reasoning_content.is_empty() {
                    warn!("model produced empty response, injecting nudge and retrying");
                    session.append_user_message(
                        "Your previous response was empty. Please provide a \
                         tool call or text response and try again.",
                    );
                    continue;
                }

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
                                 \n  1. Use `/compact` to summarize the conversation and free space\n                                 \n  2. Start a fresh session with `/reset`\n                                 \n  3. Increase the context window in your model server\n",
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
                    return Err(
                        AgentError::ProtocolViolation("empty tool_calls".to_string()).into(),
                    );
                }

                // Execute each tool call sequentially. All results are appended
                // before the session is re-sent to the model on the next
                // loop iteration.
                for call in calls {
                    if cancel.is_cancelled() {
                        return Err(AgentError::Cancelled.into());
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
                            // Feed the error back to the model as a tool
                            // result so it can see what went wrong and retry.
                            // This mirrors the denial and stuck-loop paths,
                            // which also continue the loop instead of
                            // terminating.
                            warn!(error = %e, "tool execution failed, feeding error back to model");
                            let _ = session
                                .append_tool_result(call_id, &ToolResult::error(format!("{e}")));
                            state = AgentState::Thinking;
                            continue;
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

/// Send a streaming chat request with exponential backoff on transient errors.
///
/// Uses [`ChatClient::chat_stream`] — the default implementation wraps
/// [`ChatClient::chat`], so this works for all providers. Real SSE streaming
/// is used when the provider overrides `chat_stream`.
///
/// # Errors
///
/// Returns [`RhoError::RetryBudgetExhausted`] when the retry budget is exceeded.
/// Returns any non-retryable [`RhoError`] immediately.
async fn send_with_retry_streaming(
    session: &mut Session,
    client: &dyn ChatClient,
    config: &AgentConfig,
) -> Result<AssistantResponse> {
    info!("send_with_retry_streaming called - starting retry loop");
    let mut attempts = 0u32;
    let mut last_error: Option<RhoError> = None;
    loop {
        match send_streaming(session, client).await {
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

/// Build tool calls from streaming response.
fn build_tool_calls(tc_list: &[AccumulatedToolCall]) -> Result<Vec<ModelToolCall>> {
    let mut tool_calls = Vec::new();
    for tc in tc_list {
        let id = tc.id.clone().ok_or_else(|| {
            crate::error::RhoError::Agent(AgentError::ProtocolViolation(
                "tool call missing id".to_string(),
            ))
        })?;
        let name = tc.function_name.clone().ok_or_else(|| {
            crate::error::RhoError::Agent(AgentError::ProtocolViolation(
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

/// Execute a single streaming request, consume the stream, and build an
/// [`AssistantResponse`].
///
/// For providers that use the default `chat_stream` (which wraps `chat`),
/// the entire response arrives as a single batch of chunks. For providers
/// with real SSE, chunks arrive incrementally.
async fn send_streaming(
    session: &mut Session,
    client: &dyn ChatClient,
) -> Result<AssistantResponse> {
    let fitted = session.path_messages();
    info!(
        "creating streaming request: model={}, messages={}, tools={}",
        session.model,
        fitted.len(),
        session.tools.len()
    );

    let request = ChatRequest {
        model: session.model.clone(),
        messages: fitted,
        tools: session.tools.clone(),
        stream: true,
        max_tokens: Some(session.token_budget().completion_reserve),
    };

    let request_json = serde_json::to_string(&request).unwrap_or_else(|e| {
        error!("failed to serialize request: {}", e);
        format!("{{\"serialization_error\":\"{e}\"}}")
    });
    debug!("request JSON: {}", request_json);

    let mut stream = client.chat_stream(request).await?;

    let mut chunks: Vec<StreamChunk> = Vec::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(chunk) => {
                debug!("Received stream chunk: {:?}", chunk);
                chunks.push(chunk);
            }
            Err(e) => {
                warn!("Stream chunk error: {e}");
                return Err(e);
            }
        }
    }
    debug!("Stream ended with {} chunks", chunks.len());

    let acc = StreamChunk::accumulate(&chunks);

    match acc.finish_reason {
        FinishReason::ToolCalls => {
            let tool_calls = build_tool_calls(&acc.tool_calls)?;
            // Persist assistant message with tool_calls BEFORE returning.
            session.append_assistant_message(crate::ChatMessage::Assistant {
                content: if acc.text.is_empty() {
                    vec![]
                } else {
                    vec![crate::message::ContentBlock::Text {
                        text: acc.text.clone(),
                    }]
                },
                tool_calls: tool_calls.clone(),
            });
            Ok(AssistantResponse::ToolCalls(tool_calls))
        }
        FinishReason::Length => {
            // Persist the truncated response so conversation history stays valid.
            session.append_assistant_message(crate::ChatMessage::assistant_text(&acc.text));
            Ok(AssistantResponse::LengthTruncated {
                content: acc.text,
                reasoning_content: acc.reasoning,
            })
        }
        _ => {
            // Stop, ContentFilter, or Other — treat as a message.
            let text = acc.text.clone();
            let reasoning = acc.reasoning.clone();

            // llama.cpp sometimes reports "stop" instead of "length"
            // when the model exhausts its completion budget and produces
            // nothing. Route to LengthTruncated so the agent loop can
            // attempt compaction and retry. ContentFilter is excluded
            // because retrying a filtered response is futile.
            if text.is_empty() && !matches!(acc.finish_reason, FinishReason::ContentFilter) {
                warn!(
                    finish_reason = ?acc.finish_reason,
                    "model returned empty content — treating as length truncation"
                );
                session.append_assistant_message(crate::ChatMessage::assistant_text(&text));
                return Ok(AssistantResponse::LengthTruncated {
                    content: text,
                    reasoning_content: reasoning,
                });
            }

            session.append_assistant_message(crate::ChatMessage::assistant_text(&text));
            Ok(AssistantResponse::Message {
                text: acc.text,
                reasoning_content: acc.reasoning,
            })
        }
    }
}
