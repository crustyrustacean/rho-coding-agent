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

/// A token budget for context window management.
///
/// Phase 1a uses a character-count heuristic (≈ 4 chars per token).
#[derive(Clone, Copy, Debug)]
pub struct TokenBudget {
    /// Maximum number of tokens to include in a request.
    pub max_tokens: usize,
}

impl TokenBudget {
    /// Create a token budget.
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self { max_tokens: 8_192 }
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
        let available = budget.max_tokens.saturating_sub(system_tokens);

        let turn_tokens: Vec<usize> = turns
            .iter()
            .map(|t| t.iter().map(approximate_tokens).sum())
            .collect();

        let total: usize = turn_tokens.iter().sum();
        let mut excess = total.saturating_sub(available);
        let mut drop = 0;

        for &t in &turn_tokens {
            if excess == 0 {
                break;
            }
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
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
