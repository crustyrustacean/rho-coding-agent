//! Shared test infrastructure for the rho workspace.
//!
//! # Contents
//!
//! - [`MockChatClient`] — a [`ChatClient`] that returns canned [`ModelResponse`]
//!   values and records every [`ChatRequest`] it receives.
//! - [`MockShellExecutor`] — a [`ShellExecutor`] that returns canned [`ShellOutput`]
//!   values and records every command it receives.
//! - [`FileTestEnv`] — a temporary directory with a [`SandboxRoot`] and helpers
//!   for creating files and subdirectories inside the sandbox.
//! - [`detect_shell`] — detect the available PowerShell executable (`pwsh` or
//!   `powershell`), returning `None` if neither is on `PATH`.
//! - [`load_fixture`] — load a JSON fixture file from `tests/fixtures/`.
//!
//! Add this crate as a `dev-dependency`; it is never published.
//!
//! [`ChatClient`]: rho_core::ChatClient
//! [`ChatRequest`]: rho_core::ChatRequest
//! [`ModelResponse`]: rho_core::ModelResponse
//! [`ShellExecutor`]: rho_core::ShellExecutor
//! [`ShellOutput`]: rho_core::ShellOutput

use async_trait::async_trait;
use rho_core::{
    ChatClient, ChatRequest, ModelResponse, RhoError, SandboxRoot, ShellExecutor, ShellOutput,
    TrustStore, approval::ApprovalGate, message::ModelToolCall, tool::ToolRisk,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub use tempfile::TempDir;

// ── MockChatClient ────────────────────────────────────────────────────────────

/// A [`ChatClient`] that returns pre-loaded results in sequence.
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
impl ChatClient for MockChatClient {
    async fn chat(&self, request: ChatRequest) -> rho_core::Result<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        let mut items = self.items.lock().unwrap();
        assert!(
            !items.is_empty(),
            "MockChatClient: no more canned results — check test setup"
        );
        items.remove(0)
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
}

impl MockShellExecutor {
    /// Create a mock executor that returns the given outputs in sequence.
    pub fn new(outputs: Vec<ShellOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs)),
            commands: Arc::new(Mutex::new(Vec::new())),
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
}

#[async_trait]
impl ShellExecutor for MockShellExecutor {
    async fn execute(
        &self,
        command: &str,
        _working_dir: &Path,
        _timeout: Option<Duration>,
        _cancel: rho_core::CancellationToken,
    ) -> rho_core::Result<ShellOutput> {
        self.commands.lock().unwrap().push(command.to_owned());
        let mut outputs = self.outputs.lock().unwrap();
        assert!(
            !outputs.is_empty(),
            "MockShellExecutor: no more canned outputs — check test setup"
        );
        Ok(outputs.remove(0))
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
