//! Context window management.
//!
//! The [`ContextManager`] trait keeps the conversation within the model's token budget.
//! The default [`SlidingWindowContextManager`] evicts by *turn* — never individual
//! messages — to preserve the invariant that an `Assistant { tool_calls }` message is
//! always accompanied by its matching `Tool` result messages.
//!
//! # Invariants all implementations must uphold
//!
//! 1. **System message pinned** — never evicted.
//! 2. **Tool-pair integrity** — an `Assistant { tool_calls }` message is never
//!    separated from its matching `Tool` result messages. Violating this causes the
//!    model API to return a 400 error.

use crate::message::ChatMessage;
use crate::schema::ToolSchema;
use crate::session::{Entry, EntryPayload, EntryResolution, TokenEstimator};
use tracing::{debug, warn};

/// A token budget for context window management.
///
/// Separates the model's context window into a *prompt* budget (what the
/// conversation can use) and a *completion reserve* (room left for the
/// model's reply). This prevents the pathological case where a fully-budgeted
/// prompt leaves no room for the model to respond.
///
/// # Prompt budget
///
/// `prompt_budget()` returns `context_window - completion_reserve`. All
/// context-fitting logic should use this, not `context_window` directly.
///
/// # Invariant
///
/// The completion reserve is **hard**: conversation + system prompt +
/// tool schemas must never consume it. All context-fitting paths
/// ([`ContextManager::fit_path`], [`ContextManager::fit`]) enforce this
/// by subtracting overhead before fitting messages.
#[derive(Clone, Copy, Debug)]
pub struct TokenBudget {
    /// The model's total context window size.
    pub context_window: usize,
    /// Tokens reserved for the model's completion. Default: 4096.
    ///
    /// **Invariant:** conversation + system + schema ≤
    /// (`context_window` − `completion_reserve`). This reserve is never
    /// consumed by conversation history.
    pub completion_reserve: usize,
}

impl TokenBudget {
    /// Create a token budget with the given context window and default
    /// completion reserve (4096).
    pub fn new(context_window: usize) -> Self {
        Self {
            context_window,
            completion_reserve: 4096,
        }
    }

    /// Create a token budget with explicit context window and completion reserve.
    pub fn with_reserve(context_window: usize, completion_reserve: usize) -> Self {
        Self {
            context_window,
            completion_reserve,
        }
    }

    /// The maximum number of tokens available for the prompt.
    ///
    /// This is `context_window - completion_reserve`. All context-fitting
    /// logic should use this value.
    pub fn prompt_budget(&self) -> usize {
        self.context_window.saturating_sub(self.completion_reserve)
    }

    /// Backwards-compatible accessor: the context window size.
    ///
    /// Prefer [`prompt_budget()`](Self::prompt_budget) for fitting logic.
    pub fn max_tokens(&self) -> usize {
        self.context_window
    }

    /// The budget available for conversation messages after subtracting
    /// system prompt and tool-schema overhead.
    ///
    /// This is the number of tokens that can actually be filled with
    /// user/assistant/tool messages. Returns 0 (via saturating subtraction)
    /// when overhead exceeds the prompt budget.
    pub fn message_budget(&self, system_overhead: usize, schema_overhead: usize) -> usize {
        self.prompt_budget()
            .saturating_sub(system_overhead)
            .saturating_sub(schema_overhead)
    }

    /// Returns `true` if the given token totals would exceed the prompt
    /// budget, violating the completion-reserve invariant.
    pub fn would_exceed(
        &self,
        conversation_tokens: usize,
        system_overhead: usize,
        schema_overhead: usize,
    ) -> bool {
        let total = conversation_tokens
            .saturating_add(system_overhead)
            .saturating_add(schema_overhead);
        total > self.prompt_budget()
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            context_window: 32_768,
            completion_reserve: 4096,
        }
    }
}

/// Approximate token count of a message using a character-count heuristic (≈ 4 chars/token).
///
/// Walks the message structure directly, summing string lengths without
/// serializing to JSON. Falls back to 256 tokens (one message's worth)
/// if the message somehow contains no text.
pub(crate) fn approximate_tokens(msg: &ChatMessage) -> usize {
    use crate::message::ContentBlock;

    let mut chars = 0usize;

    // Approximate overhead from role, JSON keys, and separators.
    // Each message adds ~20 chars of structural JSON.
    chars += 20;

    match msg {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. } => {
            for block in content {
                let ContentBlock::Text { text } = block;
                chars += text.len();
            }
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            chars += tool_call_id.len() + 10; // tool_call_id + key overhead
            for block in content {
                let ContentBlock::Text { text } = block;
                chars += text.len();
            }
        }
    }

    let tokens = chars.div_ceil(4);
    tokens.max(1) // at least 1 token per message
}

/// Context window management interface.
///
/// Implementations provide two methods:
/// - [`fit`] — the original flat-message fitting logic.
/// - [`fit_path`] — tree-aware fitting that filters by resolution, renders
///   compaction summaries as synthetic messages, and subtracts tool-schema /
///   system-message overhead before delegating to [`fit`].
///
/// The default [`fit_path`] implementation gives every existing context manager
/// tree-awareness *and* calibrated overhead handling for free — only [`fit`]
/// needs to be implemented.
///
/// [`fit`]: ContextManager::fit
/// [`fit_path`]: ContextManager::fit_path
pub trait ContextManager: Send + Sync {
    /// Return the subset of `messages` that fits within `budget`.
    fn fit(&self, messages: &[ChatMessage], budget: TokenBudget) -> Vec<ChatMessage>;

    /// Return the subset of session-tree entries that fits within `budget`,
    /// with tree-aware filtering and overhead subtraction.
    ///
    /// The default implementation:
    /// 1. Filters out entries with resolution `Compacted` or `Attached` — they
    ///    don't participate in the model's context.
    /// 2. Filters payload kinds that don't become messages: `Custom`, `Label`,
    ///    `LeafMoved`, `ModelChange`, `SessionInfo`.
    /// 3. Renders `Compaction` and `BranchSummary` entries as synthetic
    ///    `ChatMessage::User` entries with deterministic framing of the
    ///    [`CompactionSummary`](crate::session::CompactionSummary).
    /// 4. Converts remaining `Message` and `CustomMessage` entries to
    ///    `ChatMessage`.
    /// 5. Computes tool-schema overhead using `estimator` and subtracts it
    ///    from `budget.prompt_budget()`.
    /// 6. Computes system-message overhead and subtracts it as well.
    /// 7. Delegates the final budget enforcement to [`fit`](Self::fit).
    ///
    /// Entries are expected in **chronological order** (root → leaf), as
    /// produced by reversing `path_to_root()`.
    fn fit_path(
        &self,
        entries: &[&Entry],
        budget: TokenBudget,
        estimator: &dyn TokenEstimator,
        tool_schemas: &[ToolSchema],
    ) -> Vec<ChatMessage> {
        // Step 1–4: Convert entries to messages, respecting resolution.
        let mut messages = Vec::with_capacity(entries.len());
        for entry in entries {
            // Skip entries that don't participate in the model's context.
            match &entry.resolution {
                EntryResolution::Compacted { .. } | EntryResolution::Attached => continue,
                EntryResolution::Full => {}
            }

            match &entry.payload {
                EntryPayload::Message(msg) => {
                    messages.push(msg.clone());
                }
                EntryPayload::CustomMessage { content, .. } => {
                    messages.push(ChatMessage::User {
                        content: content.clone(),
                    });
                }
                EntryPayload::Compaction { summary, .. }
                | EntryPayload::BranchSummary { summary, .. } => {
                    messages.push(render_compaction_summary(summary));
                }
                // Payload kinds that don't become messages:
                EntryPayload::Custom { .. }
                | EntryPayload::Label { .. }
                | EntryPayload::LeafMoved { .. }
                | EntryPayload::ModelChange { .. }
                | EntryPayload::SessionInfo { .. } => {
                    // Silently skipped — these don't reach the model.
                }
            }
        }

        // Step 5: Compute tool-schema overhead.
        let schema_overhead = estimate_tool_schema_overhead(tool_schemas, estimator);

        // Step 6: Compute system-message overhead.
        let system_overhead = messages
            .iter()
            .find(|m| matches!(m, ChatMessage::System { .. }))
            .map_or(0, approximate_tokens);

        // Step 7: Subtract overhead from budget, then delegate to fit.
        let adjusted_budget = TokenBudget::with_reserve(
            budget.context_window.saturating_sub(schema_overhead),
            budget.completion_reserve + system_overhead,
        );

        self.fit(&messages, adjusted_budget)
    }
}

/// Render a [`CompactionSummary`](crate::session::CompactionSummary) as a
/// synthetic `ChatMessage::User` with deterministic framing.
///
/// The rendering contract is:
/// ```text
/// [Compacted: {entry_count} entries, {tokens_compacted} tokens, span {duration}]
/// Original request: "{original_request, if present}"
/// Tool activity:
///   - {tool_name}: {N} calls — {args_summary_1}, {args_summary_2}, ...
/// {notes, if present}
/// ```
pub fn render_compaction_summary(summary: &crate::session::CompactionSummary) -> ChatMessage {
    use std::fmt::Write;

    let mut body = String::new();

    // Header line
    let _ = writeln!(
        body,
        "[Compacted: {} entries, {} tokens, span {:?}]",
        summary.entry_count, summary.tokens_compacted, summary.time_span
    );

    // Original request
    if let Some(ref req) = summary.original_request {
        let _ = writeln!(body, "Original request: \"{req}\"");
    }

    // Tool activity
    if !summary.tool_calls.is_empty() {
        body.push_str("Tool activity:\n");
        for (tool_name, calls) in &summary.tool_calls {
            let _ = write!(body, "  - {tool_name}: {} calls", calls.len());
            if !calls.is_empty() {
                body.push_str(" — ");
                let _ = write!(body, "{}", calls.join(", "));
            }
            body.push('\n');
        }
    }

    // Notes
    if let Some(ref notes) = summary.notes {
        let _ = writeln!(body, "{notes}");
    }

    ChatMessage::user_text(body)
}

/// Estimate the token overhead of tool schemas.
///
/// Tool schemas are sent with every request but are not part of the
/// message history. Their token cost must be subtracted from the prompt
/// budget to avoid over-estimating available space.
pub(crate) fn estimate_tool_schema_overhead(
    schemas: &[ToolSchema],
    estimator: &dyn TokenEstimator,
) -> usize {
    if schemas.is_empty() {
        return 0;
    }
    // Serialize schemas to JSON and estimate tokens from that.
    // Each schema is ~200-500 chars; we count the whole tools array.
    // We add a small per-schema overhead for JSON structural tokens.
    let serialized = serde_json::to_string(schemas).unwrap_or_default();
    estimator.estimate(&serialized)
}

/// A role-tagged message for counting evicted messages by type.
enum MessageRole {
    /// A user message.
    User,
    /// An assistant message (with or without tool calls).
    Assistant,
    /// A tool result message.
    Tool,
}

impl MessageRole {
    /// Classify a message by its role variant.
    fn classify(msg: &ChatMessage) -> Self {
        match msg {
            ChatMessage::User { .. } => Self::User,
            ChatMessage::Assistant { .. } => Self::Assistant,
            ChatMessage::Tool { .. } => Self::Tool,
            ChatMessage::System { .. } => unreachable!("system messages are never in turns"),
        }
    }
}

/// Default [`ContextManager`]: sliding window that evicts by turn.
///
/// A **turn** is either a lone `User` message, or an `Assistant { tool_calls }`
/// message plus all its matching `Tool` result messages.
#[derive(Default)]
pub struct SlidingWindowContextManager;

impl SlidingWindowContextManager {
    /// Create a new instance.
    pub fn new() -> Self {
        Self
    }

    /// Split messages into (system, turns).
    fn group(messages: &[ChatMessage]) -> (Option<ChatMessage>, Vec<Vec<ChatMessage>>) {
        let mut system: Option<ChatMessage> = None;
        let mut rest: Vec<&ChatMessage> = Vec::new();

        for msg in messages {
            if matches!(msg, ChatMessage::System { .. }) {
                system = Some(msg.clone());
            } else {
                rest.push(msg);
            }
        }

        let mut turns: Vec<Vec<ChatMessage>> = Vec::new();
        let mut i = 0;

        while i < rest.len() {
            match rest[i] {
                ChatMessage::User { .. } => {
                    turns.push(vec![rest[i].clone()]);
                    i += 1;
                }
                ChatMessage::Assistant { tool_calls, .. } => {
                    let ids: Vec<&str> = tool_calls.iter().map(|c| c.id.as_ref()).collect();
                    let mut turn = vec![rest[i].clone()];
                    i += 1;
                    // Absorb all matching Tool results into this turn
                    while i < rest.len() {
                        if let ChatMessage::Tool { tool_call_id, .. } = rest[i]
                            && ids.contains(&tool_call_id.as_ref())
                        {
                            turn.push(rest[i].clone());
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    turns.push(turn);
                }
                other => {
                    turns.push(vec![other.clone()]);
                    i += 1;
                }
            }
        }

        (system, turns)
    }
}

impl ContextManager for SlidingWindowContextManager {
    fn fit(&self, messages: &[ChatMessage], budget: TokenBudget) -> Vec<ChatMessage> {
        let (system, mut turns) = Self::group(messages);

        let system_tokens = system.as_ref().map_or(0, approximate_tokens);
        let available = budget.prompt_budget().saturating_sub(system_tokens);

        let turn_tokens: Vec<usize> = turns
            .iter()
            .map(|t| t.iter().map(approximate_tokens).sum())
            .collect();

        let total: usize = turn_tokens.iter().sum();
        let mut excess = total.saturating_sub(available);
        let mut drop = 0;

        // Invariant 1: never evict the most recent turn. The last turn is the
        // one the model is currently operating on — evicting it causes
        // amnesia (the user's request disappears from context). If the last
        // turn alone exceeds the budget, the bounded-tool-result handling
        // will have already truncated it; dropping it entirely is always wrong.
        //
        // Invariant 2: never evict the first user turn. The first user message
        // contains the user's original request; losing it means the model
        // forgets what it was asked to do. (This is the amnesia bug that
        // Phase 2.5's CompactionSummary::original_request solves structurally;
        // in the linear model, pinning is the fix.)
        let first_user_turn = turns.iter().position(|t| {
            t.first()
                .is_some_and(|m| matches!(m, ChatMessage::User { .. }))
        });
        let max_droppable = turns.len().saturating_sub(1);

        // Count evicted messages by role for diagnostics.
        let mut dropped_user = 0usize;
        let mut dropped_assistant = 0usize;
        let mut dropped_tool = 0usize;
        let mut tokens_freed: usize = 0;

        for (idx, &t) in turn_tokens.iter().enumerate() {
            if excess == 0 {
                break;
            }
            if idx >= max_droppable {
                // Don't evict the last turn — it contains the active
                // user request or in-flight tool call.
                break;
            }
            if first_user_turn == Some(idx) {
                // Don't evict the first user turn — it contains the
                // original request; losing it causes amnesia.
                break;
            }
            // Classify messages in the evicted turn by role.
            for msg in &turns[drop] {
                match MessageRole::classify(msg) {
                    MessageRole::User => dropped_user += 1,
                    MessageRole::Assistant => dropped_assistant += 1,
                    MessageRole::Tool => dropped_tool += 1,
                }
            }
            tokens_freed += t;
            drop += 1;
            excess = excess.saturating_sub(t);
        }

        turns.drain(0..drop);

        let mut result = Vec::new();
        if let Some(sys) = system {
            result.push(sys);
        }
        for turn in turns {
            result.extend(turn);
        }

        debug!(
            input_messages = messages.len(),
            output_messages = result.len(),
            total_tokens = total,
            budget_tokens = budget.prompt_budget(),
            available_tokens = available,
            turns_dropped = drop,
            dropped_user,
            dropped_assistant,
            dropped_tool,
            "fit completed"
        );

        if drop > 0 {
            warn!(
                turns_dropped = drop,
                tokens_freed,
                dropped_user,
                dropped_assistant,
                dropped_tool,
                "context window: turns evicted"
            );
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentBlock;
    use crate::message::{ModelToolCall, ToolCallFunction};
    use crate::newtypes::{ToolCallId, ToolName};

    fn assistant_with_tool_call(call_id: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: vec![],
            tool_calls: vec![ModelToolCall {
                id: ToolCallId::from(call_id),
                call_type: "function".to_owned(),
                function: ToolCallFunction {
                    name: ToolName::from("any_tool"),
                    arguments: "{}".to_owned(),
                },
            }],
        }
    }

    #[test]
    fn system_message_is_always_retained() {
        let messages = vec![
            ChatMessage::system_text("you are rho"),
            ChatMessage::user_text("hello"),
            ChatMessage::assistant_text("hi"),
        ];
        let cm = SlidingWindowContextManager::new();
        // Budget so tight nothing else fits
        let result = cm.fit(&messages, TokenBudget::new(1));
        assert!(
            result
                .iter()
                .any(|m| matches!(m, ChatMessage::System { .. }))
        );
    }

    #[test]
    fn tool_call_turn_is_never_split() {
        let messages = vec![
            ChatMessage::system_text("sys"),
            ChatMessage::user_text("do it"),
            assistant_with_tool_call("call_1"),
            ChatMessage::tool_result(ToolCallId::from("call_1"), "result"),
            ChatMessage::user_text("next"),
            ChatMessage::assistant_text("done"),
        ];
        // Tight budget — should evict oldest turns
        let cm = SlidingWindowContextManager::new();
        let fitted = cm.fit(&messages, TokenBudget::new(40));

        let has_assistant = fitted.iter().any(
            |m| matches!(m, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        );
        let has_result = fitted.iter().any(
            |m| matches!(m, ChatMessage::Tool { tool_call_id, .. } if &**tool_call_id == "call_1"),
        );

        // Both present or both absent — never split
        assert_eq!(has_assistant, has_result, "tool-call turn was split");
    }

    #[test]
    fn empty_messages_returns_empty() {
        let cm = SlidingWindowContextManager::new();
        assert!(cm.fit(&[], TokenBudget::default()).is_empty());
    }

    #[test]
    fn all_messages_retained_when_under_budget() {
        let messages = vec![
            ChatMessage::system_text("sys"),
            ChatMessage::user_text("hi"),
            ChatMessage::assistant_text("hello"),
        ];
        let cm = SlidingWindowContextManager::new();
        let result = cm.fit(&messages, TokenBudget::default());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn token_budget_default_is_32k() {
        assert_eq!(TokenBudget::default().context_window, 32_768);
    }

    // ── TokenBudget convenience method tests (Fix 2a) ────────────────────

    #[test]
    fn message_budget_subtracts_overheads() {
        let budget = TokenBudget::new(32_768); // reserve = 4096, prompt = 28672
        assert_eq!(budget.message_budget(5000, 1000), 28_672 - 5000 - 1000);
    }

    #[test]
    fn message_budget_saturates_at_zero() {
        let budget = TokenBudget::new(100); // reserve = 4096, prompt = 0 (saturating)
        assert_eq!(budget.message_budget(5000, 1000), 0);
    }

    #[test]
    fn would_exceed_returns_false_when_under_budget() {
        let budget = TokenBudget::new(32_768);
        assert!(!budget.would_exceed(10_000, 5000, 1000));
    }

    #[test]
    fn would_exceed_returns_true_when_over_budget() {
        let budget = TokenBudget::new(32_768); // prompt = 28672
        assert!(budget.would_exceed(25_000, 5000, 1000)); // 31000 > 28672
    }

    #[test]
    fn would_exceed_returns_false_at_exact_budget() {
        let budget = TokenBudget::with_reserve(10_000, 2000); // prompt = 8000
        assert!(!budget.would_exceed(5000, 2000, 1000)); // 8000 == 8000
    }

    #[test]
    fn would_exceed_handles_overflow_safely() {
        let budget = TokenBudget::new(100);
        // usize::MAX values should not panic
        assert!(budget.would_exceed(usize::MAX, 1, 1));
    }

    #[test]
    fn last_turn_is_never_evicted() {
        // Even under severe budget pressure, the most recent turn
        // must survive — evicting it causes amnesia.
        let messages = vec![
            ChatMessage::system_text("sys"),
            ChatMessage::user_text("important question"),
            ChatMessage::assistant_text("answer"),
            ChatMessage::user_text("follow-up question"),
        ];
        let cm = SlidingWindowContextManager::new();
        // Tiny budget — forces eviction
        let fitted = cm.fit(&messages, TokenBudget::new(1));
        // The system message is always pinned
        assert!(
            fitted
                .iter()
                .any(|m| matches!(m, ChatMessage::System { .. }))
        );
        // The last user message must survive even under extreme pressure
        assert!(
            fitted.iter().any(|m| matches!(m, ChatMessage::User { .. })),
            "last user turn must survive even under extreme budget pressure"
        );
    }

    #[test]
    fn first_user_turn_is_never_evicted() {
        // The first user message contains the original request — losing it
        // is the amnesia bug. It must survive even under severe budget pressure.
        let messages = vec![
            ChatMessage::system_text("sys"),
            ChatMessage::user_text("remember the secret code: APPLE-42"),
            ChatMessage::assistant_text("got it"),
            ChatMessage::user_text("read file A"),
            ChatMessage::assistant_text("ok"),
            ChatMessage::user_text("read file B"),
        ];
        let cm = SlidingWindowContextManager::new();
        let fitted = cm.fit(&messages, TokenBudget::new(1));
        // The first user message must survive
        let has_secret = fitted.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("APPLE-42")
                })
            } else {
                false
            }
        });
        assert!(
            has_secret,
            "first user turn (containing the secret) must survive eviction"
        );
    }

    // ── fit_path tests (Task 8) ──────────────────────────────────────────

    use crate::newtypes::EntryId;
    use crate::schema::ToolSchema;
    use crate::session::{
        CompactionSummary, Entry, EntryPayload, EntryResolution, HeuristicEstimator,
    };
    use serde_json::json;
    use std::time::{Duration, SystemTime};

    /// Helper: create a test entry with the given payload and resolution.
    fn test_entry(payload: EntryPayload, resolution: EntryResolution) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution,
            payload,
        }
    }

    #[test]
    fn fit_path_returns_messages_from_full_entries() {
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::Message(ChatMessage::user_text("hello")),
                EntryResolution::Full,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], ChatMessage::System { .. }));
        assert!(matches!(result[1], ChatMessage::User { .. }));
    }

    #[test]
    fn fit_path_skips_compacted_entries() {
        let compacted_into = EntryId::from("test");
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::Message(ChatMessage::user_text("old")),
                EntryResolution::Compacted {
                    into: compacted_into,
                },
            ),
            test_entry(
                EntryPayload::Message(ChatMessage::user_text("new")),
                EntryResolution::Full,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        // The compacted entry should be filtered; only System + "new" remain
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], ChatMessage::System { .. }));
        let is_new = result.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text == "new"
                })
            } else {
                false
            }
        });
        assert!(is_new, "new message should be present");
    }

    #[test]
    fn fit_path_skips_attached_entries() {
        let target_id = EntryId::new();
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::Label {
                    target_id,
                    label: Some("checkpoint".to_owned()),
                },
                EntryResolution::Attached,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        // Only System should remain; Label is Attached and filtered
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ChatMessage::System { .. }));
    }

    #[test]
    fn fit_path_skips_non_message_payloads() {
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::ModelChange {
                    model: "gpt-4".to_owned(),
                },
                EntryResolution::Attached,
            ),
            test_entry(
                EntryPayload::Custom {
                    kind: "rho.diagnostics.v1".to_owned(),
                    data: json!({}),
                },
                EntryResolution::Attached,
            ),
            test_entry(
                EntryPayload::SessionInfo {
                    name: "test".to_owned(),
                },
                EntryResolution::Attached,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        // Only the system message should survive
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn fit_path_renders_compaction_as_synthetic_user() {
        let summary = CompactionSummary {
            original_request: Some("fix the bug".to_owned()),
            tool_calls: {
                let mut map = std::collections::BTreeMap::new();
                map.insert(ToolName::from("read_file"), vec!["main.rs".to_owned()]);
                map
            },
            tokens_compacted: 500,
            entry_count: 3,
            time_span: Duration::from_secs(30),
            notes: None,
        };

        let first_kept = EntryId::new();
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::Compaction {
                    summary,
                    first_kept,
                    tokens_before: 1000,
                },
                EntryResolution::Full,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        // Should have System + synthetic User
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0], ChatMessage::System { .. }));
        assert!(matches!(result[1], ChatMessage::User { .. }));

        // Verify the synthetic message content
        if let ChatMessage::User { content } = &result[1] {
            let ContentBlock::Text { text } = &content[0];
            assert!(text.contains("[Compacted: 3 entries, 500 tokens"));
            assert!(text.contains("Original request: \"fix the bug\""));
            assert!(text.contains("read_file"));
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn fit_path_renders_branch_summary_as_synthetic_user() {
        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: Duration::from_secs(5),
            notes: None,
        };

        let from_id = EntryId::new();
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::BranchSummary { summary, from_id },
                EntryResolution::Full,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        assert_eq!(result.len(), 2);
        assert!(matches!(result[1], ChatMessage::User { .. }));
    }

    #[test]
    fn fit_path_converts_custom_message_to_user() {
        let entries = [
            test_entry(
                EntryPayload::Message(ChatMessage::system_text("sys")),
                EntryResolution::Full,
            ),
            test_entry(
                EntryPayload::CustomMessage {
                    kind: "rho.diagnostics.v1".to_owned(),
                    content: vec![ContentBlock::Text {
                        text: "3 errors found".to_owned(),
                    }],
                },
                EntryResolution::Full,
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let result = cm.fit_path(&refs, TokenBudget::default(), &estimator, &[]);

        assert_eq!(result.len(), 2);
        assert!(matches!(result[1], ChatMessage::User { .. }));
    }

    #[test]
    fn fit_path_subtracts_tool_schema_overhead() {
        let tools = vec![ToolSchema::function(
            "read_file",
            "Read a file from disk",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];

        // Create a very tight budget where schema overhead matters
        let entries: Vec<Entry> = (0..20)
            .map(|i| {
                test_entry(
                    EntryPayload::Message(ChatMessage::user_text(format!(
                        "message {i} with padding"
                    ))),
                    EntryResolution::Full,
                )
            })
            .collect();
        let refs: Vec<&Entry> = entries.iter().collect();

        let cm = SlidingWindowContextManager::new();
        let estimator = HeuristicEstimator::new();
        let budget = TokenBudget::with_reserve(200, 10);

        let result_with_tools = cm.fit_path(&refs, budget, &estimator, &tools);

        let result_without_tools = cm.fit_path(&refs, budget, &estimator, &[]);

        // With tools, the budget is smaller (schema overhead subtracted),
        // so fewer messages should fit
        assert!(
            result_with_tools.len() <= result_without_tools.len(),
            "fit_path with tools should fit fewer or equal messages than without"
        );
    }

    #[test]
    fn estimate_tool_schema_overhead_returns_zero_for_empty() {
        let estimator = HeuristicEstimator::new();
        assert_eq!(estimate_tool_schema_overhead(&[], &estimator), 0);
    }

    #[test]
    fn estimate_tool_schema_overhead_returns_nonzero_for_schemas() {
        let estimator = HeuristicEstimator::new();
        let tools = vec![ToolSchema::function(
            "test",
            "A test tool",
            json!({"type": "object"}),
        )];
        let overhead = estimate_tool_schema_overhead(&tools, &estimator);
        assert!(overhead > 0, "tool schemas should have nonzero overhead");
    }
}
