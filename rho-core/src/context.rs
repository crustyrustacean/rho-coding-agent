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
#[derive(Clone, Copy, Debug)]
pub struct TokenBudget {
    /// The model's total context window size.
    pub context_window: usize,
    /// Tokens reserved for the model's completion. Default: 4096.
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
fn approximate_tokens(msg: &ChatMessage) -> usize {
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
pub trait ContextManager: Send + Sync {
    /// Return the subset of `messages` that fits within `budget`.
    fn fit(&self, messages: &[ChatMessage], budget: TokenBudget) -> Vec<ChatMessage>;
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
        // in the linear Conversation model, pinning is the fix.)
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
}
