//! Shared test infrastructure for the rho workspace.
//!
//! # Contents
//!
//! - [`MockChatClient`] — an [`LlmService`](rho_ai::LlmService) that returns canned [`ModelResponse`]
//!   values and records every [`ChatRequest`] it receives.
//! - [`MockShellExecutor`] — a [`ShellExecutor`] that returns canned [`ShellOutput`]
//!   values and records every command it receives.
//! - [`FixedResponseTool`] — a [`Tool`] that always returns a fixed string,
//!   with configurable name, response, and risk level. Use [`fixed_registry`]
//!   to build a [`ToolRegistry`] containing a single `FixedResponseTool`.
//! - [`FileTestEnv`] — a temporary directory with a [`SandboxRoot`] and helpers
//!   for creating files and subdirectories inside the sandbox.
//! - [`single_text_turn`] — run a single agent loop turn (text-only).
//! - [`single_tool_turn`] — run a single agent loop turn (tool call + text).
//! - [`detect_shell`] — detect the available PowerShell executable (`pwsh` or
//!   `powershell`), returning `None` if neither is on `PATH`.
//! - [`load_fixture`] — load a JSON fixture file from `tests/fixtures/`.
//! - [`assert_no_orphan_tool_results`] — assert every `Tool` message has a
//!   matching preceding `Assistant` tool call.
//!
//! Add this crate as a `dev-dependency`; it is never published.
//!
//! [`LlmService`]: rho_ai::LlmService
//! [`ChatRequest`]: rho_core::ChatRequest
//! [`ModelResponse`]: rho_core::ModelResponse
//! [`ShellExecutor`]: rho_core::ShellExecutor
//! [`ShellOutput`]: rho_core::ShellOutput

use async_trait::async_trait;
use rho_ai::{EventStream, LlmRequest, LlmService, ProviderError};
use rho_core::{
    AgentConfig, CancellationToken, ChatMessage, ChatRequest, ModelResponse, RhoError, SandboxRoot,
    Session, ShellExecutor, ShellOutput, Tool, ToolName, ToolOutcome, ToolRegistry, ToolResult,
    TrustStore,
    agent::{LoopParams, NopObserver, run_loop},
    approval::ApprovalGate,
    message::ModelToolCall,
    tool::ToolRisk,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub use tempfile::TempDir;

// ── MockChatClient ────────────────────────────────────────────────────────────

/// Convert a `ModelResponse` into a sequence of `StreamEvent`s.
///
/// Used by `MockChatClient`'s `LlmService` implementation to produce
/// events from canned `ModelResponse` fixtures.
fn model_response_to_events(response: &ModelResponse) -> Vec<rho_ai::StreamEvent> {
    let mut events = Vec::new();

    if let Some(choice) = response.choices.first() {
        if !choice.message.content.is_empty() {
            events.push(rho_ai::StreamEvent::Text(choice.message.content.clone()));
        }
        if !choice.message.reasoning_content.is_empty() {
            events.push(rho_ai::StreamEvent::Reasoning(
                choice.message.reasoning_content.clone(),
            ));
        }
        for (index, tc) in choice.message.tool_calls.iter().enumerate() {
            events.push(rho_ai::StreamEvent::ToolUseStart {
                index,
                id: tc.id.to_string(),
                name: tc.function.name.to_string(),
            });
            events.push(rho_ai::StreamEvent::ToolUseInputDelta {
                index,
                delta: tc.function.arguments.clone(),
            });
            events.push(rho_ai::StreamEvent::ToolUseComplete {
                index,
                tool_call: rho_ai::ToolCall {
                    id: tc.id.to_string(),
                    name: tc.function.name.to_string(),
                    arguments: tc.function.arguments.clone(),
                },
            });
        }
        let stop_reason = match &choice.finish_reason {
            rho_core::FinishReason::Stop => rho_ai::StopReason::EndTurn,
            rho_core::FinishReason::ToolCalls => rho_ai::StopReason::ToolUse,
            rho_core::FinishReason::Length => rho_ai::StopReason::Length,
            rho_core::FinishReason::ContentFilter => rho_ai::StopReason::ContentFilter,
            rho_core::FinishReason::Other(s) => rho_ai::StopReason::Other(s.clone()),
        };
        events.push(rho_ai::StreamEvent::Done {
            reason: stop_reason,
            usage: rho_ai::StreamUsage::new(
                response.usage.prompt_tokens as u64,
                response.usage.completion_tokens as u64,
            ),
        });
    }

    events
}

/// Convert a [`RhoError`] to a [`ProviderError`], preserving retryability.
///
/// This is used by `MockChatClient`'s `LlmService` implementation to
/// convert stored `RhoError` values back into appropriate `ProviderError`
/// variants so that the agent loop's retry logic works correctly.
fn convert_error_to_provider(e: RhoError) -> ProviderError {
    match e {
        RhoError::Client(client_err) => match client_err {
            rho_core::client::error::ClientError::Http(http_err) => {
                ProviderError::Http { source: http_err }
            }
            rho_core::client::error::ClientError::HttpError { status, message } => {
                ProviderError::HttpStatus {
                    status,
                    body: Some(message),
                    retryable: matches!(status, 429 | 500 | 502 | 503 | 504),
                }
            }
            rho_core::client::error::ClientError::RetryBudgetExhausted(_attempts, last) => {
                ProviderError::RetryBudgetExhausted {
                    last_error: Box::new(convert_error_to_provider(rho_core::RhoError::Client(
                        *last,
                    ))),
                }
            }
            _ => ProviderError::Sse {
                message: client_err.to_string(),
            },
        },
        _ => ProviderError::Sse {
            message: e.to_string(),
        },
    }
}

/// An [`LlmService`](rho_ai::LlmService) that returns pre-loaded results in sequence.
///
/// Records every [`ChatRequest`] it receives so tests can inspect the full
/// conversation that would have been sent to a real model API.
///
/// Use [`MockChatClient::new`] for simple success-response sequences, or
/// [`MockChatClient::with_results`] to mix successes and errors (e.g. for
/// retry tests).
///
/// # Panics
///
/// Panics if called more times than there are queued results. This is
/// intentional — an under-queued mock is a test-setup bug and should fail
/// loudly rather than producing a confusing downstream error.
#[derive(Clone)]
pub struct MockChatClient {
    /// Queued results (success or error) returned in order.
    items: Arc<Mutex<Vec<Result<ModelResponse, RhoError>>>>,
    /// All requests received, in order.
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

impl MockChatClient {
    /// Create a client with a sequence of successful responses.
    ///
    /// Responses are returned in order: the first call returns `responses[0]`,
    /// the second call returns `responses[1]`, and so on.
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            items: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a client with a sequence of results (successes and errors).
    ///
    /// Use this for retry tests where you need to queue retryable errors
    /// followed by a successful response.
    pub fn with_results(items: Vec<Result<ModelResponse, RhoError>>) -> Self {
        Self {
            items: Arc::new(Mutex::new(items)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// All requests that have been sent to this client, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmService for MockChatClient {
    async fn chat_stream(
        &self,
        request: LlmRequest,
    ) -> std::result::Result<EventStream, ProviderError> {
        // Convert LlmRequest → ChatRequest for internal storage.
        let chat_request = ChatRequest {
            model: request.model,
            messages: request
                .messages
                .into_iter()
                .map(|m| match m {
                    rho_ai::LlmMessage::System(text) => ChatMessage::system_text(text),
                    rho_ai::LlmMessage::User(text) => ChatMessage::user_text(text),
                    rho_ai::LlmMessage::Assistant {
                        content,
                        tool_calls,
                    } => {
                        let text = content.unwrap_or_default();
                        let tcs: Vec<ModelToolCall> = tool_calls
                            .into_iter()
                            .map(|tc| ModelToolCall {
                                id: rho_core::ToolCallId::new(tc.id),
                                call_type: "function".to_owned(),
                                function: rho_core::ToolCallFunction {
                                    name: rho_core::ToolName::new(tc.name),
                                    arguments: tc.arguments,
                                },
                            })
                            .collect();
                        ChatMessage::Assistant {
                            content: if text.is_empty() {
                                vec![]
                            } else {
                                vec![rho_core::ContentBlock::Text { text }]
                            },
                            tool_calls: tcs,
                        }
                    }
                    rho_ai::LlmMessage::Tool {
                        tool_call_id,
                        content,
                    } => ChatMessage::tool_result(rho_core::ToolCallId::new(tool_call_id), content),
                })
                .collect(),
            tools: request
                .tools
                .into_iter()
                .map(|t| rho_core::ToolSchema::function(t.name, t.description, t.parameters))
                .collect(),
            stream: true,
            max_tokens: request.max_tokens,
        };

        // Record the request
        self.requests.lock().unwrap().push(chat_request.clone());

        // Get the canned response and convert to StreamEvents
        let mut items = self.items.lock().unwrap();
        assert!(
            !items.is_empty(),
            "MockChatClient: no more canned results — check test setup"
        );
        let result = items.remove(0);

        match result {
            Ok(response) => {
                let events = model_response_to_events(&response);
                Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
            }
            Err(e) => {
                // Convert RhoError to ProviderError, preserving retryability.
                let provider_err = convert_error_to_provider(e);
                Err(provider_err)
            }
        }
    }
}

// ── MockShellExecutor ───────────────────────────────────────────────────────

/// A [`ShellExecutor`] that returns canned [`ShellOutput`] values.
///
/// Records every command it receives so tests can inspect what the tool
/// layer asked the shell to run. Used by `RunCommand` unit tests that need
/// to exercise argument parsing and result formatting without spawning a
/// real shell.
///
/// # Panics
///
/// Panics if called more times than there are queued outputs. This is
/// intentional — an under-queued mock is a test-setup bug.
#[derive(Clone)]
pub struct MockShellExecutor {
    /// Queued outputs returned in order.
    outputs: Arc<Mutex<Vec<ShellOutput>>>,
    /// All commands received, in order.
    commands: Arc<Mutex<Vec<String>>>,
    /// All working directories received, in order.
    working_dirs: Arc<Mutex<Vec<std::path::PathBuf>>>,
}

impl MockShellExecutor {
    /// Create a mock executor that returns the given outputs in sequence.
    pub fn new(outputs: Vec<ShellOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs)),
            commands: Arc::new(Mutex::new(Vec::new())),
            working_dirs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// All commands that have been sent to this executor, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// All working directories that have been passed to this executor, in order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn working_dirs(&self) -> Vec<std::path::PathBuf> {
        self.working_dirs.lock().unwrap().clone()
    }
}

#[async_trait]
impl ShellExecutor for MockShellExecutor {
    async fn execute(
        &self,
        command: &str,
        working_dir: &Path,
        _timeout: Option<Duration>,
        _cancel: rho_core::CancellationToken,
        _input: Option<&str>,
    ) -> rho_core::Result<ShellOutput> {
        self.commands.lock().unwrap().push(command.to_owned());
        self.working_dirs
            .lock()
            .unwrap()
            .push(working_dir.to_path_buf());
        let mut outputs = self.outputs.lock().unwrap();
        assert!(
            !outputs.is_empty(),
            "MockShellExecutor: no more canned outputs — check test setup"
        );
        Ok(outputs.remove(0))
    }
}

// ── FixedResponseTool ───────────────────────────────────────────────────

/// A tool that always returns a fixed string. Used in integration tests
/// as a stand-in for any tool the model might call.
///
/// The name, response text, and risk level are all configurable, making
/// this a universal replacement for stub tools scattered across test files.
pub struct FixedResponseTool {
    /// The tool's registered name.
    pub name: &'static str,
    /// The fixed text to return on every invocation.
    pub response: String,
    /// The risk classification reported to the approval policy.
    pub risk: ToolRisk,
}

#[async_trait]
impl Tool for FixedResponseTool {
    fn name(&self) -> ToolName {
        ToolName::from(self.name)
    }

    fn description(&self) -> &str {
        "fixed response tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn risk(&self) -> ToolRisk {
        self.risk
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        Ok(ToolOutcome::Immediate(ToolResult::success(&self.response)))
    }
}

/// Build a [`ToolRegistry`] containing a single [`FixedResponseTool`].
pub fn fixed_registry(name: &'static str, response: String, risk: ToolRisk) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(FixedResponseTool {
        name,
        response,
        risk,
    }));
    reg
}

/// A tool that always returns an error. Used to test error handling
/// in the agent loop (e.g., tool execution failure mid-batch).
pub struct FailingTool {
    pub name: &'static str,
    pub error_message: String,
}

#[async_trait]
impl Tool for FailingTool {
    fn name(&self) -> ToolName {
        ToolName::from(self.name)
    }

    fn description(&self) -> &str {
        "failing tool"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        Err(rho_core::RhoError::Tool(self.error_message.clone()))
    }
}

// ── Response builders ─────────────────────────────────────────────────────────

/// Build a minimal [`ModelResponse`] that returns a text message.
///
/// Useful for building mock sequences without writing out the full JSON structure.
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn text_response(text: impl Into<String>) -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": text.into(),
                "reasoning_content": "",
                "tool_calls": []
            },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("text_response: invalid fixture")
}

/// Build a minimal [`ModelResponse`] that requests a single tool call.
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn tool_call_response(
    call_id: impl Into<String>,
    tool_name: impl Into<String>,
    arguments: impl Into<String>,
) -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "",
                "tool_calls": [{
                    "id": call_id.into(),
                    "type": "function",
                    "function": {
                        "name": tool_name.into(),
                        "arguments": arguments.into()
                    }
                }]
            },
            "logprobs": null,
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("tool_call_response: invalid fixture")
}

/// Build a minimal [`ModelResponse`] that requests multiple tool calls.
///
/// Each tuple is `(call_id, tool_name, arguments)`.
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn multi_tool_call_response(
    calls: Vec<(impl Into<String>, impl Into<String>, impl Into<String>)>,
) -> ModelResponse {
    let tool_calls: Vec<serde_json::Value> = calls
        .into_iter()
        .map(|(id, name, args)| {
            serde_json::json!({
                "id": id.into(),
                "type": "function",
                "function": {
                    "name": name.into(),
                    "arguments": args.into()
                }
            })
        })
        .collect();

    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "",
                "tool_calls": tool_calls
            },
            "logprobs": null,
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("multi_tool_call_response: invalid fixture")
}

/// Build a [`ModelResponse`] with `finish_reason: "length"` and the given content.
///
/// Both `content` and `reasoning_content` default to `""` if not provided.
/// This models a reasoning model that ran out of tokens (e.g. spent everything
/// on chain-of-thought with no content output).
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn length_truncated_response(
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content.into(),
                "reasoning_content": reasoning_content.into(),
                "tool_calls": []
            },
            "logprobs": null,
            "finish_reason": "length"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("length_truncated_response: invalid fixture")
}

/// Build a [`ModelResponse`] where the model claims `finish_reason: "stop"`
/// but returns empty content.
///
/// This models the llama.cpp behaviour where the server reports "stop"
/// instead of "length" when the model exhausts its completion budget.
/// Build a [`ModelResponse`] where the model claims `finish_reason: "stop"`
/// but returns empty content.
///
/// This models the llama.cpp behaviour where the server reports "stop"
/// instead of "length" when the model exhausts its completion budget.
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn empty_stop_response() -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "",
                "tool_calls": []
            },
            "logprobs": null,
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("empty_stop_response: invalid fixture")
}

/// Build a [`ModelResponse`] with `finish_reason: "content_filter"` and
/// empty content.
/// Build a [`ModelResponse`] with `finish_reason: "content_filter"` and
/// empty content.
///
/// # Panics
///
/// Panics if the internal fixture JSON is malformed (should never happen).
pub fn empty_content_filter_response() -> ModelResponse {
    let json = serde_json::json!({
        "id": "mock-id",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "",
                "tool_calls": []
            },
            "logprobs": null,
            "finish_reason": "content_filter"
        }],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        },
        "stats": {},
        "system_fingerprint": ""
    });
    serde_json::from_value(json).expect("empty_content_filter_response: invalid fixture")
}

// ── Fixture loader ────────────────────────────────────────────────────────────

/// Load a fixture file relative to the calling crate's `tests/fixtures/` directory.
///
/// `path` is relative to the crate root, e.g.
/// `"tests/fixtures/responses/chat_completion.json"`.
///
/// # Panics
///
/// Panics if the file cannot be read.
pub fn load_fixture(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("load_fixture({path}): {e}"))
}

// ── Approval helpers ──────────────────────────────────────────────────────────

/// An [`ApprovalGate`] that approves every tool call without prompting.
///
/// Use in tests that exercise the agent loop but do not care about approval.
pub struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn request_approval(&self, _call: &ModelToolCall, _risk: ToolRisk) -> bool {
        true
    }
}

/// An [`ApprovalGate`] that denies every tool call.
///
/// Use in tests that verify denial handling.
pub struct AutoDenyGate;

#[async_trait]
impl ApprovalGate for AutoDenyGate {
    async fn request_approval(&self, _call: &ModelToolCall, _risk: ToolRisk) -> bool {
        false
    }
}

// ── File-system test environment ──────────────────────────────────────────────

/// A temporary directory with a [`SandboxRoot`] and convenience methods for
/// creating files and subdirectories inside the sandbox.
///
/// Use this in integration tests that need a real file system inside a
/// sandbox. The [`TempDir`] is deleted when dropped.
///
/// # Examples
///
/// ```ignore
/// let env = FileTestEnv::new();
/// env.write_file("src/main.rs", "fn main() {}");
/// env.create_dir("src/lib");
/// // sandbox root is env.root(), temp dir lives in env.temp_dir()
/// ```
///
/// # Panics
///
/// Panics if the temp directory or sandbox root cannot be created, or if
/// file/directory creation fails.
pub struct FileTestEnv {
    /// Temporary directory that is deleted on drop.
    dir: TempDir,
    /// Sandbox root validated against this directory.
    root: SandboxRoot,
}

impl FileTestEnv {
    /// Create a new temporary directory with a sandbox rooted at it.
    ///
    /// # Panics
    ///
    /// Panics if the temp directory or sandbox root cannot be created.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = SandboxRoot::new(dir.path()).expect("create sandbox root");
        Self { dir, root }
    }

    /// The sandbox root path.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The underlying [`SandboxRoot`].
    pub fn sandbox(&self) -> &SandboxRoot {
        &self.root
    }

    /// The underlying [`TempDir`]. Keep it alive for the test duration.
    pub fn temp_dir(&self) -> &TempDir {
        &self.dir
    }

    /// Create a file with the given contents relative to the sandbox root.
    ///
    /// Parent directories are created automatically.
    ///
    /// # Panics
    ///
    /// Panics if the file or parent directories cannot be created.
    pub fn write_file(&self, relative_path: &str, contents: &str) {
        let path = self.dir.path().join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create parent dirs for {relative_path}: {e}"));
        }
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {relative_path}: {e}"));
    }

    /// Create a subdirectory relative to the sandbox root.
    ///
    /// Parent directories are created automatically.
    ///
    /// # Panics
    ///
    /// Panics if the directory cannot be created.
    pub fn create_dir(&self, relative_path: &str) {
        let path = self.dir.path().join(relative_path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("create dir {relative_path}: {e}"));
    }

    /// Read a file relative to the sandbox root.
    ///
    /// # Panics
    ///
    /// Panics if the file cannot be read.
    pub fn read_file(&self, relative_path: &str) -> String {
        let path = self.dir.path().join(relative_path);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {relative_path}: {e}"))
    }

    /// Check whether a file or directory exists relative to the sandbox root.
    pub fn exists(&self, relative_path: &str) -> bool {
        self.dir.path().join(relative_path).exists()
    }
}

impl Default for FileTestEnv {
    fn default() -> Self {
        Self::new()
    }
}

// ── Sandbox helpers ───────────────────────────────────────────────────────────

/// Create a temporary directory and a [`SandboxRoot`] rooted at it.
///
/// The [`TempDir`] must be kept alive for the duration of the test; it is
/// deleted when dropped.
///
/// For richer file-system test support, use [`FileTestEnv`] instead.
///
/// # Panics
///
/// Panics if the temp directory or sandbox root cannot be created.
pub fn tempdir_with_sandbox() -> (TempDir, SandboxRoot) {
    let env = FileTestEnv::new();
    // Unwrap the env into its components for backward compatibility.
    let dir = env.dir;
    let root = env.root;
    (dir, root)
}

// ── Trust-store helpers ───────────────────────────────────────────────────────

/// Create an isolated [`TrustStore`] backed by a file in a temporary directory.
///
/// Returns both the [`TrustStore`] and the [`TempDir`] keeping it alive.
/// The store is empty (no pre-trusted files).
///
/// # Panics
///
/// Panics if the temp directory cannot be created.
pub fn empty_trust_store() -> (TrustStore, TempDir) {
    let dir = tempfile::tempdir().expect("create tempdir for trust store");
    let path = dir.path().join("trusted_projects.toml");
    let store = TrustStore::load_from(&path);
    (store, dir)
}

/// Return a path for an isolated trust-store file inside `dir`.
///
/// Use this when you need to pre-populate the store before loading it.
pub fn trust_store_path(dir: &TempDir) -> PathBuf {
    dir.path().join("trusted_projects.toml")
}

// ── Shell detection ────────────────────────────────────────────────────────────

/// Detect the available PowerShell executable on the system.
///
/// Returns `"pwsh"` if PowerShell 7+ is on `PATH`, `"powershell"` if
/// Windows PowerShell 5.1 is on `PATH`, or `None` if neither is found.
///
/// Use this in integration tests that spawn real PowerShell processes
/// to skip tests gracefully when no shell is available, rather than
/// panicking as [`PowerShellExecutor::new()`] does.
///
/// [`PowerShellExecutor::new()`]: rho_tools::PowerShellExecutor
pub fn detect_shell() -> Option<&'static str> {
    if which_exists("pwsh") {
        Some("pwsh")
    } else if which_exists("powershell") {
        Some("powershell")
    } else {
        None
    }
}

/// Check whether an executable exists on `PATH`.
///
/// Mirrors the private `which_exists` in `rho-tools` so test code can
/// detect shells without depending on `rho-tools` (which would create a
/// circular dev-dependency).
fn which_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

// ── Session helpers ──────────────────────────────────────────────────────────

/// Create an in-memory session suitable for testing.
///
/// Uses [`Session::in_memory`] so no disk I/O occurs. The model is set to
/// `"mock"` and the CWD to `/tmp`.
///
/// If `system_prompt` is `None`, a default `"you are a test assistant"` is used.
pub fn in_memory_session(
    system_prompt: Option<&str>,
    tools: Vec<rho_ai::ToolDefinition>,
) -> Session {
    let prompt = system_prompt.unwrap_or("you are a test assistant");
    Session::in_memory("mock", Some(prompt), tools, "/tmp")
}

// ── Assertion helpers ─────────────────────────────────────────────────────────

/// Assert that no `Tool` message exists without a preceding `Assistant` message
/// containing the matching `tool_call_id`.
///
/// This is the structural invariant that the agent loop must uphold:
/// every tool result in the conversation must reference a tool call from
/// the immediately preceding assistant message.
///
/// # Panics
///
/// Panics with a descriptive message if any `Tool` message is an "orphan"
/// (no matching assistant tool call before it).
pub fn assert_no_orphan_tool_results(messages: &[ChatMessage]) {
    let mut prev_was_assistant_with_calls = false;
    let mut prev_call_ids: Vec<&str> = Vec::new();

    for msg in messages {
        match msg {
            ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                prev_was_assistant_with_calls = true;
                prev_call_ids = tool_calls.iter().map(|c| c.id.as_ref()).collect();
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                assert!(
                    prev_was_assistant_with_calls,
                    "orphan Tool message with call_id '{tool_call_id}'"
                );
                assert!(
                    prev_call_ids.contains(&tool_call_id.as_ref()),
                    "Tool call_id mismatch: '{tool_call_id}' not in {prev_call_ids:?}"
                );
            }
            ChatMessage::Assistant { .. } => {
                prev_was_assistant_with_calls = false;
                prev_call_ids.clear();
            }
            ChatMessage::System { .. } | ChatMessage::User { .. } => {}
        }
    }
}

// ── Agent loop helpers ─────────────────────────────────────────────────────────

/// Run a single agent loop turn: user sends text, model responds with text.
///
/// Returns the response text from the turn.
///
/// # Panics
///
/// Panics if the agent loop fails.
pub async fn single_text_turn(
    session: &mut Session,
    user_text: &str,
    response_text: &str,
    registry: &ToolRegistry,
) -> String {
    let client = MockChatClient::new(vec![text_response(response_text)]);
    let config = AgentConfig::default();
    let params = LoopParams {
        client: &client,
        registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    run_loop(session, user_text, &params).await.unwrap()
}

/// Run a single agent loop turn: user sends text, model requests a tool call,
/// tool executes, model replies with text.
///
/// The turn completes after the model responds with text (the "done" response).
///
/// # Panics
///
/// Panics if the agent loop fails.
pub async fn single_tool_turn(
    session: &mut Session,
    user_text: &str,
    call_id: &str,
    tool_name: &str,
    tool_args: &str,
    registry: &ToolRegistry,
) {
    let client = MockChatClient::new(vec![
        tool_call_response(call_id, tool_name, tool_args),
        text_response("done"),
    ]);
    let config = AgentConfig::default();
    let params = LoopParams {
        client: &client,
        registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let _ = run_loop(session, user_text, &params).await.unwrap();
}
