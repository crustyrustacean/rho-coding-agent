//! # rho-ai
//!
//! Unified LLM provider abstraction for the rho coding agent.
//!
//! Provides a single [`LlmService`] trait backed by three provider families:
//! - **OpenAI-compatible** — `OpenAI`, `DeepSeek`, `xAI`, `Groq`, `OpenRouter`, `Ollama`, `LM Studio`, etc.
//! - **Anthropic** — `Claude` models via the Messages API
//! - **Google** — `Gemini` models via the Generative AI API
//!
//! ## Architecture
//!
//! Provider differences are confined to three modules (`openai`, `anthropic`, `google`).
//! Everything else in the crate — and all consumers — uses the unified types from [`types`]:
//!
//! - [`LlmMessage`] — chat messages (System, User, Assistant, Tool)
//! - [`ToolCall`] — tool calls from the LLM
//! - [`ToolDefinition`] — tool schemas presented to the LLM
//! - [`StreamEvent`] — response stream events (`Text`, `Reasoning`, `ToolUse*`, `Done`)
//!
//! ## Quick Start
//!
//! ```ignore
//! use rho_ai::{openai::OpenAiService, types::*, service::LlmService};
//!
//! let service = OpenAiService::new(ProviderConfig::new(
//!     "gpt-4o",
//!     env::var("OPENAI_API_KEY").unwrap(),
//!     "https://api.openai.com/v1",
//! ));
//!
//! let events = service.chat_stream_with_tools(messages, tools).await?;
//! ```

pub mod error;
pub mod openai;
pub mod retry;
pub mod service;
pub mod sse;
pub mod types;

pub use error::ProviderError;
pub use service::{EventStream, LlmService};
pub use types::{
    AccumulatedResponse, AccumulatedToolCall, Backend, LlmMessage, LlmRequest, ProviderConfig,
    StopReason, StreamEvent, StreamUsage, ToolCall, ToolDefinition,
};
