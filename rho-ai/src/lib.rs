//! # rho-ai
//!
//! Unified LLM provider abstraction for the rho coding agent.
//!
//! Provides one [`LlmService`] trait backed by two OpenAI-shaped transports:
//! - **Chat Completions** — [`openai::OpenAiService`] for compatible providers
//!   such as `OpenRouter`, `Groq`, Z.ai, `Ollama`, and `LM Studio`.
//! - **Responses** — [`responses::ResponsesService`] for native `OpenAI` text,
//!   reasoning-summary, and function-call streaming.
//!
//! ## Architecture
//!
//! Transport differences are confined to `openai` and `responses`. Everything
//! else in the crate—and all consumers—uses the unified types from [`types`]:
//!
//! - [`LlmMessage`] — chat messages (System, User, Assistant, Tool)
//! - [`ToolCall`] — tool calls from the LLM
//! - [`ToolDefinition`] — tool schemas presented to the LLM
//! - [`StreamEvent`] — response stream events (`Text`, `Reasoning`, `ToolUse*`, `Done`)
//!
//! ## Quick Start
//!
//! ```ignore
//! use rho_ai::{responses::ResponsesService, LlmMessage, LlmRequest,
//!     LlmService, ProviderConfig};
//!
//! let service = ResponsesService::new(ProviderConfig::new(
//!     env::var("OPENAI_API_KEY").unwrap(),
//!     "https://api.openai.com/v1/responses",
//! ));
//! let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
//! let events = service.chat_stream(request).await?;
//! ```

pub mod catalog;
pub mod catalog_generated;
pub mod error;
pub mod openai;
pub mod responses;
pub mod retry;
pub mod service;
pub mod sse;
pub mod types;

pub use catalog::*;
pub use error::ProviderError;
pub use service::{EventStream, LlmService};
pub use types::{
    AccumulatedResponse, AccumulatedToolCall, Backend, LlmMessage, LlmRequest, ProviderConfig,
    StopReason, StreamEvent, StreamUsage, ToolCall, ToolDefinition,
};
