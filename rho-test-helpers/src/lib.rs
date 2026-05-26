//! Shared test infrastructure for the rho workspace.
//!
//! # Contents
//!
//! - [`MockChatClient`] — an [`LlmService`](rho_ai::LlmService) that returns canned
//!   [`StreamEvent`](rho_ai::StreamEvent) sequences and records every [`LlmRequest`] it receives.
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
//! # Event builders
//!
//! The fixture builders produce `Vec<StreamEvent>` directly:
//!
//! - [`text_events`] — model responds with text, stops.
//! - [`tool_call_events`] — model requests a single tool call.
//! - [`multi_tool_call_events`] — model requests multiple tool calls.
//! - [`length_truncated_events`] — model hit token budget.
//! - [`empty_stop_events`] — model stops with empty content.
//! - [`empty_content_filter_events`] — content filtered, empty response.
//!
//! Add this crate as a `dev-dependency`; it is never published.
//!
//! [`LlmService`]: rho_ai::LlmService
//! [`ShellExecutor`]: rho_core::ShellExecutor
//! [`ShellOutput`]: rho_core::ShellOutput

use async_trait::async_trait;
use rho_ai::{EventStream, LlmRequest, LlmService, ProviderError};
use rho_core::{
    AgentConfig, CancellationToken, ChatMessage, RhoError, SandboxRoot, Session, ShellExecutor,
    ShellOutput, Tool, ToolName, ToolOutcome, ToolRegistry, ToolResult, TrustStore,
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

/// Convert a [`RhoError`] to a [`ProviderError`], preserving retryability.
///
/// Used by [`MockChatClient`] to convert stored `RhoError` values back into
/// appropriate `ProviderError` variants so that the agent loop's retry logic
/// works correctly.
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

/// A canned response for [`MockChatClient`].
///
/// Wraps either a sequence of [`StreamEvent`](rho_ai::StreamEvent)s (success)
/// or a [`RhoError`] (failure).
pub enum MockResponse {
    /// Successful response: a sequence of stream events.
    Events(Vec<rho_ai::StreamEvent>),
    /// Error response.
    Error(RhoError),
}

impl Clone for MockChatClient {
    fn clone(&self) -> Self {
        Self {
            items: Arc::clone(&self.items),
            requests: Arc::clone(&self.requests),
        }
    }
}

impl From<Vec<rho_ai::StreamEvent>> for MockResponse {
    fn from(events: Vec<rho_ai::StreamEvent>) -> Self {
        Self::Events(events)
    }
}

/// An [`LlmService`](rho_ai::LlmService) that returns pre-loaded results in sequence.
///
/// Records every [`LlmRequest`] it receives so tests can inspect what the
/// agent loop sent to the model.
///
/// Use [`MockChatClient::new`] for success-response sequences, or
/// [`MockChatClient::with_results`] to mix successes and errors.
///
/// # Panics
///
/// Panics if called more times than there are queued results. This is
/// intentional — an under-queued mock is a test-setup bug.
pub struct MockChatClient {
    /// Queued results returned in order.
    items: Arc<Mutex<Vec<MockResponse>>>,
    /// All requests received, in order.
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl MockChatClient {
    /// Create a client with a sequence of successful event lists.
    ///
    /// Each `Vec<StreamEvent>` is returned as one complete stream response.
    pub fn new(responses: Vec<Vec<rho_ai::StreamEvent>>) -> Self {
        Self {
            items: Arc::new(Mutex::new(
                responses.into_iter().map(MockResponse::Events).collect(),
            )),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a client with a sequence of [`MockResponse`] values.
    ///
    /// Use this to mix successes and errors (e.g. for retry tests).
    pub fn with_results(items: Vec<MockResponse>) -> Self {
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
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmService for MockChatClient {
    async fn chat_stream(
        &self,
        request: LlmRequest,
    ) -> std::result::Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);

        let mut items = self.items.lock().unwrap();
        assert!(
            !items.is_empty(),
            "MockChatClient: no more canned results — check test setup"
        );
        let result = items.remove(0);

        match result {
            MockResponse::Events(events) => {
                Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
            }
            MockResponse::Error(e) => {
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
/// layer asked the shell to run.
///
/// # Panics
///
/// Panics if called more times than there are queued outputs.
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

/// A tool that always returns a fixed string.
///
/// The name, response text, and risk level are all configurable, making
/// this a universal replacement for stub tools across test files.
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

// ── StreamEvent builders ──────────────────────────────────────────────────────

/// Build a stream that delivers a text response, then ends the turn.
///
/// # Examples
///
/// ```ignore
/// let client = MockChatClient::new(vec![text_events("hello")]);
/// ```
pub fn text_events(text: impl Into<String>) -> Vec<rho_ai::StreamEvent> {
    vec![
        rho_ai::StreamEvent::Text(text.into()),
        done_event(rho_ai::StopReason::EndTurn),
    ]
}

/// Build a stream that delivers a single tool call, then signals tool-use stop.
///
/// # Examples
///
/// ```ignore
/// let client = MockChatClient::new(vec![tool_call_events("c1", "echo", "{}")]);
/// ```
pub fn tool_call_events(
    call_id: impl Into<String>,
    tool_name: impl Into<String>,
    arguments: impl Into<String>,
) -> Vec<rho_ai::StreamEvent> {
    let id = call_id.into();
    let name = tool_name.into();
    let args = arguments.into();
    vec![
        rho_ai::StreamEvent::ToolUseStart {
            index: 0,
            id: id.clone(),
            name: name.clone(),
        },
        rho_ai::StreamEvent::ToolUseInputDelta {
            index: 0,
            delta: args.clone(),
        },
        rho_ai::StreamEvent::ToolUseComplete {
            index: 0,
            tool_call: rho_ai::ToolCall {
                id,
                name,
                arguments: args,
            },
        },
        done_event(rho_ai::StopReason::ToolUse),
    ]
}

/// Build a stream that delivers multiple tool calls, then signals tool-use stop.
///
/// Each tuple is `(call_id, tool_name, arguments)`.
pub fn multi_tool_call_events(
    calls: Vec<(impl Into<String>, impl Into<String>, impl Into<String>)>,
) -> Vec<rho_ai::StreamEvent> {
    let mut events = Vec::new();
    for (index, (id, name, args)) in calls.into_iter().enumerate() {
        let id = id.into();
        let name = name.into();
        let args = args.into();
        events.push(rho_ai::StreamEvent::ToolUseStart {
            index,
            id: id.clone(),
            name: name.clone(),
        });
        events.push(rho_ai::StreamEvent::ToolUseInputDelta {
            index,
            delta: args.clone(),
        });
        events.push(rho_ai::StreamEvent::ToolUseComplete {
            index,
            tool_call: rho_ai::ToolCall {
                id,
                name,
                arguments: args,
            },
        });
    }
    events.push(done_event(rho_ai::StopReason::ToolUse));
    events
}

/// Build a stream with `StopReason::Length` and the given content/reasoning.
///
/// Models a reasoning model that ran out of tokens.
pub fn length_truncated_events(
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> Vec<rho_ai::StreamEvent> {
    let mut events = Vec::new();
    let text = content.into();
    if !text.is_empty() {
        events.push(rho_ai::StreamEvent::Text(text));
    }
    let reasoning = reasoning_content.into();
    if !reasoning.is_empty() {
        events.push(rho_ai::StreamEvent::Reasoning(reasoning));
    }
    events.push(done_event(rho_ai::StopReason::Length));
    events
}

/// Build a stream with `StopReason::EndTurn` but empty content.
///
/// Models the llama.cpp behaviour where the server reports "stop"
/// instead of "length" when the model exhausts its completion budget.
pub fn empty_stop_events() -> Vec<rho_ai::StreamEvent> {
    vec![done_event(rho_ai::StopReason::EndTurn)]
}

/// Build a stream with `StopReason::ContentFilter` and empty content.
pub fn empty_content_filter_events() -> Vec<rho_ai::StreamEvent> {
    vec![done_event(rho_ai::StopReason::ContentFilter)]
}

/// Helper: build a `Done` event with zero usage.
fn done_event(reason: rho_ai::StopReason) -> rho_ai::StreamEvent {
    rho_ai::StreamEvent::Done {
        reason,
        usage: rho_ai::StreamUsage::new(0, 0),
    }
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
/// to skip tests gracefully when no shell is available.
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
    let client = MockChatClient::new(vec![text_events(response_text)]);
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
        tool_call_events(call_id, tool_name, tool_args),
        text_events("done"),
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

// ── TestProvider ─────────────────────────────────────────────────────────────

/// A [`Provider`](rho_core::Provider) that wraps a [`MockChatClient`].
///
/// Use this in integration tests that need a `ProviderRegistry` with a
/// working LLM service but no live model server. The provider reports
/// itself as local (not external) with no discoverable models.
///
/// # Examples
///
/// ```ignore
/// let client = MockChatClient::new(vec![text_events("hello")]);
/// let provider = TestProvider::new("test", client);
/// let mut registry = ProviderRegistry::new();
/// registry.add(Box::new(provider));
/// ```
pub struct TestProvider {
    /// Provider name.
    name: String,
    /// The mock LLM service.
    client: MockChatClient,
}

impl TestProvider {
    /// Create a new test provider with the given name and mock client.
    pub fn new(name: &str, client: MockChatClient) -> Self {
        Self {
            name: name.to_owned(),
            client,
        }
    }
}

#[async_trait]
impl rho_core::Provider for TestProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_external(&self) -> bool {
        false
    }

    async fn list_models(&self) -> rho_core::Result<rho_core::ModelList> {
        Ok(rho_core::ModelList {
            data: vec![rho_core::ModelInfo {
                id: "mock-model".to_owned(),
                object: "model".to_owned(),
                created: 0,
                owned_by: "test".to_owned(),
            }],
        })
    }

    fn llm_service(&self) -> &dyn rho_ai::LlmService {
        &self.client
    }

    fn clone_boxed_service(&self) -> Box<dyn rho_ai::LlmService> {
        Box::new(self.client.clone())
    }
}
