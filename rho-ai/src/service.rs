//! The [`LlmService`] trait — the core abstraction for provider-agnostic LLM communication.

use crate::error::ProviderError;
use crate::types::StreamEvent;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// A stream of LLM response events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// Provider-agnostic interface for chatting with an LLM.
///
/// Implementations wrap provider-specific HTTP clients and SSE parsers.
/// The caller provides unified types (`LlmMessage`, `ToolDefinition`) and
/// receives a stream of unified `StreamEvent` variants, regardless of which
/// provider is backing the call.
///
/// Each provider module provides its own factory function to construct
/// an implementation of this trait from a [`crate::types::ProviderConfig`].
#[async_trait]
pub trait LlmService: Send + Sync {
    /// Send a chat completion request and stream the response.
    ///
    /// The request carries the model, messages, tools, and optional limits.
    /// The LLM may respond with text, tool calls, or both.
    /// Tool calls arrive as a sequence of `StreamEvent` variants:
    /// `ToolUseStart` → zero or more `ToolUseInputDelta` → `ToolUseComplete`.
    async fn chat_stream(
        &self,
        request: crate::types::LlmRequest,
    ) -> Result<EventStream, ProviderError>;
}
