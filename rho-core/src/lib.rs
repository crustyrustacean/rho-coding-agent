//! rho-core — the agent kernel.
//!
//! Provides the data model, traits, and agent loop that everything else builds on.
//!
//! # Module layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`error`] | [`RhoError`] and [`Result`] |
//! | [`newtypes`] | [`FilePath`], [`ToolName`], [`ToolCallId`], [`DiagnosticCode`] |
//! | [`message`] | [`ChatMessage`], [`ContentBlock`], [`ModelToolCall`] |
//! | [`schema`] | [`ToolSchema`] — wire-format tool definitions for API requests |
//! | [`request`] | [`ChatRequest`] |
//! | [`response`] | [`ModelResponse`], [`FinishReason`], etc. |
//! | [`tool`] | [`Tool`] trait, [`ToolRegistry`], [`ToolRisk`], [`ToolResult`], [`CancellationToken`] |
//! | [`client`] | [`ChatClient`] trait, [`LocalChatClient`] |
//! | [`context`] | [`ContextManager`] trait, [`SlidingWindowContextManager`], [`TokenBudget`] |
//! | [`conversation`] | [`Conversation`], [`AssistantResponse`] |
//! | [`agent`] | [`AgentState`], [`AgentConfig`], [`run_loop`] |
//! | [`prompts`] | [`base_prompt()`] |
//!
//! # Newtype Deref policy
//!
//! Domain newtypes implement `Deref` to their inner type so call sites don't need
//! `.0` access:
//! - [`FilePath`] → `Deref<Target = Path>`
//! - [`ToolName`], [`ToolCallId`], [`DiagnosticCode`] → `Deref<Target = str>`
//!
//! [`base_prompt()`]: prompts::base_prompt
//! [`FilePath`]: newtypes::FilePath
//! [`ToolName`]: newtypes::ToolName
//! [`ToolCallId`]: newtypes::ToolCallId
//! [`DiagnosticCode`]: newtypes::DiagnosticCode
//! [`RhoError`]: error::RhoError
//! [`Result`]: error::Result
//! [`ChatMessage`]: message::ChatMessage
//! [`ContentBlock`]: message::ContentBlock
//! [`ModelToolCall`]: message::ModelToolCall
//! [`ToolSchema`]: schema::ToolSchema
//! [`ChatRequest`]: request::ChatRequest
//! [`ModelResponse`]: response::ModelResponse
//! [`FinishReason`]: response::FinishReason
//! [`Tool`]: tool::Tool
//! [`ToolRegistry`]: tool::ToolRegistry
//! [`ToolRisk`]: tool::ToolRisk
//! [`ToolResult`]: tool::ToolResult
//! [`CancellationToken`]: tool::CancellationToken
//! [`ChatClient`]: client::ChatClient
//! [`LocalChatClient`]: client::LocalChatClient
//! [`ContextManager`]: context::ContextManager
//! [`SlidingWindowContextManager`]: context::SlidingWindowContextManager
//! [`TokenBudget`]: context::TokenBudget
//! [`Conversation`]: conversation::Conversation
//! [`AssistantResponse`]: conversation::AssistantResponse
//! [`AgentState`]: agent::AgentState
//! [`AgentConfig`]: agent::AgentConfig
//! [`run_loop`]: agent::run_loop

pub mod agent;
pub mod approval;
pub mod client;
pub mod context;
pub mod context_files;
pub mod conversation;
pub mod error;
pub mod message;
pub mod newtypes;
pub mod prompts;
pub mod redact;
pub mod request;
pub mod response;
pub mod sandbox;
pub mod schema;
pub mod tool;

// Convenience re-exports for the most commonly used types
pub use agent::{AgentConfig, AgentState, TransitionError, run_loop};
pub use approval::{ApprovalGate, ApprovalPolicy, AutoApprovePolicy, DefaultApprovalPolicy};
pub use client::{ChatClient, LocalChatClient};
pub use context::{ContextManager, SlidingWindowContextManager, TokenBudget};
pub use context_files::{ContextFile, ContextScanner, TrustStore, compose_system_prompt};
pub use conversation::{AssistantResponse, Conversation};
pub use error::{Result, RhoError};
pub use message::{ChatMessage, ContentBlock, ModelToolCall, ToolCallFunction};
pub use newtypes::{DiagnosticCode, FilePath, ToolCallId, ToolName};
pub use prompts::base_prompt;
pub use redact::Redactor;
pub use request::ChatRequest;
pub use response::{FinishReason, ModelChoice, ModelResponse, ModelUsage};
pub use sandbox::SandboxRoot;
pub use schema::ToolSchema;
pub use tool::{CancellationToken, Tool, ToolOutcome, ToolRegistry, ToolResult, ToolRisk};
