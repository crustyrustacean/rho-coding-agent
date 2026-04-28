//! [`Conversation`] — message history and send primitives.

use crate::client::ChatClient;
use crate::context::{ContextManager, SlidingWindowContextManager, TokenBudget};
use crate::error::Result;
use crate::message::{ChatMessage, ModelToolCall};
use crate::newtypes::ToolCallId;
use crate::redact::Redactor;
use crate::request::ChatRequest;
use crate::response::FinishReason;
use crate::schema::ToolSchema;
use crate::tool::ToolResult;

/// The result of sending messages to the model.
#[derive(Debug)]
pub enum AssistantResponse {
    /// The model completed with a text reply.
    Message(String),
    /// The model requested one or more tool invocations.
    ///
    /// The agent loop should execute the tools and call
    /// [`Conversation::submit_tool_results`].
    ToolCalls(Vec<ModelToolCall>),
}

/// Manages message history and sends requests to the model.
///
/// # Persistence guarantee (task 7 fix)
///
/// When the model responds with tool calls, the assistant message — including its
/// `tool_calls` field — is persisted into history *before* returning. The subsequent
/// [`submit_tool_results`] call appends the matching `Tool` result messages. This
/// ordering is required by the `OpenAI` API: a `tool` role message must be immediately
/// preceded by an `assistant` message containing the matching `tool_call_id`.
///
/// [`submit_tool_results`]: Conversation::submit_tool_results
pub struct Conversation {
    /// Model identifier.
    model: String,
    /// Accumulated message history.
    messages: Vec<ChatMessage>,
    /// Tool schemas sent with every request.
    tools: Vec<ToolSchema>,
    /// Context window manager applied before each request.
    context_manager: Box<dyn ContextManager>,
    /// Token budget for the context manager.
    token_budget: TokenBudget,
    /// Secret redactor applied to tool results before they enter history.
    redactor: Redactor,
}

impl Conversation {
    /// Create a new conversation with the default [`SlidingWindowContextManager`].
    pub fn new(
        model: impl Into<String>,
        system_prompt: Option<&str>,
        tools: Vec<ToolSchema>,
    ) -> Self {
        let messages = match system_prompt {
            Some(p) => vec![ChatMessage::system_text(p)],
            None => vec![],
        };
        Self {
            model: model.into(),
            messages,
            tools,
            context_manager: Box::new(SlidingWindowContextManager::new()),
            token_budget: TokenBudget::default(),
            redactor: Redactor::new(),
        }
    }

    /// Override the context manager.
    #[must_use]
    pub fn with_context_manager(mut self, cm: Box<dyn ContextManager>) -> Self {
        self.context_manager = cm;
        self
    }

    /// Override the token budget.
    #[must_use]
    pub fn with_token_budget(mut self, budget: TokenBudget) -> Self {
        self.token_budget = budget;
        self
    }

    // ── Public history accessors ──────────────────────────────────────────

    /// All messages in conversation history (including the system message).
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The system prompt text, if one was set.
    pub fn system_prompt(&self) -> Option<&str> {
        self.messages.iter().find_map(|m| {
            if let ChatMessage::System { content } = m {
                content.first().map(|b| {
                    let crate::message::ContentBlock::Text { text } = b;
                    text.as_str()
                })
            } else {
                None
            }
        })
    }

    /// Replace the system prompt (or set one if none existed).
    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        if let Some(m) = self
            .messages
            .iter_mut()
            .find(|m| matches!(m, ChatMessage::System { .. }))
        {
            *m = ChatMessage::system_text(prompt);
        } else {
            self.messages.insert(0, ChatMessage::system_text(prompt));
        }
    }

    /// The current model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Switch to a different model.
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    /// Clear conversation history, retaining the system message.
    pub fn clear(&mut self) {
        self.messages
            .retain(|m| matches!(m, ChatMessage::System { .. }));
    }

    // ── Low-level push helpers (pub(crate) for agent loop) ────────────────

    /// Append a user text message.
    pub(crate) fn push_user_text(&mut self, text: &str) {
        self.messages.push(ChatMessage::user_text(text));
    }

    /// Append a tool result message, applying secret redaction first.
    ///
    /// Public so the agent loop (which lives in a sibling module) and integration
    /// tests can feed tool results back into the conversation.
    pub fn push_tool_result(&mut self, id: ToolCallId, result: &ToolResult) {
        // Redact secrets before the tool output enters conversation history.
        let redacted = self.redactor.redact(&result.output);
        self.messages.push(ChatMessage::tool_result(id, redacted));
    }

    // ── Send primitives ───────────────────────────────────────────────────

    /// Append a user message and send the conversation to the model.
    ///
    /// Convenience wrapper around [`push_user_text`] + [`send_current`].
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the HTTP request fails or the response cannot be parsed.
    ///
    /// [`push_user_text`]: Conversation::push_user_text
    /// [`send_current`]: Conversation::send_current
    /// [`RhoError`]: crate::error::RhoError
    pub async fn send(
        &mut self,
        message: &str,
        client: &dyn ChatClient,
    ) -> Result<AssistantResponse> {
        self.push_user_text(message);
        self.send_current(client).await
    }

    /// Append tool results and continue the conversation.
    ///
    /// Used by the agent loop after executing tools returned by
    /// [`AssistantResponse::ToolCalls`].
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the HTTP request fails or the response cannot be parsed.
    ///
    /// [`RhoError`]: crate::error::RhoError
    pub async fn submit_tool_results(
        &mut self,
        results: Vec<(ToolCallId, ToolResult)>,
        client: &dyn ChatClient,
    ) -> Result<AssistantResponse> {
        for (id, result) in &results {
            self.push_tool_result(id.clone(), result);
        }
        self.send_current(client).await
    }

    /// Send the current message history to the model (no new message added).
    ///
    /// Applies the context manager, builds the [`ChatRequest`], calls the client,
    /// and persists the assistant response into history.
    ///
    /// [`ChatRequest`]: crate::request::ChatRequest
    pub(crate) async fn send_current(
        &mut self,
        client: &dyn ChatClient,
    ) -> Result<AssistantResponse> {
        let fitted = self.context_manager.fit(&self.messages, self.token_budget);

        let request = ChatRequest {
            model: self.model.clone(),
            messages: fitted,
            tools: self.tools.clone(),
        };

        let response = client.chat(request).await?;
        let choice = &response.choices[0];

        if let FinishReason::ToolCalls = choice.finish_reason {
            let tool_calls = choice.message.tool_calls.clone();
            // Task 7 fix: persist assistant message with tool_calls BEFORE returning.
            // A subsequent `tool` role message requires a preceding assistant message
            // with a matching `tool_call_id`; skipping this step causes a 400.
            self.messages.push(ChatMessage::Assistant {
                content: if choice.message.content.is_empty() {
                    vec![]
                } else {
                    vec![crate::message::ContentBlock::Text {
                        text: choice.message.content.clone(),
                    }]
                },
                tool_calls: tool_calls.clone(),
            });
            Ok(AssistantResponse::ToolCalls(tool_calls))
        } else {
            let text = choice.message.content.clone();
            self.messages.push(ChatMessage::assistant_text(&text));
            Ok(AssistantResponse::Message(text))
        }
    }
}
