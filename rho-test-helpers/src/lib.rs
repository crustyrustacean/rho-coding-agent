//! Shared test infrastructure for the rho workspace.
//!
//! # Contents
//!
//! - [`MockChatClient`] — a [`ChatClient`] that returns canned [`ModelResponse`]
//!   values and records every [`ChatRequest`] it receives.
//! - [`load_fixture`] — load a JSON fixture file from `tests/fixtures/`.
//!
//! Add this crate as a `dev-dependency`; it is never published.
//!
//! [`ChatClient`]: rho_core::ChatClient
//! [`ChatRequest`]: rho_core::ChatRequest
//! [`ModelResponse`]: rho_core::ModelResponse

use async_trait::async_trait;
use rho_core::{
    ChatClient, ChatRequest, ModelResponse, RhoError, SandboxRoot, TrustStore,
    approval::ApprovalGate, message::ModelToolCall, tool::ToolRisk,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
pub use tempfile::TempDir;

// ── MockChatClient ────────────────────────────────────────────────────────────

/// A [`ChatClient`] that returns pre-loaded responses in sequence.
///
/// Records every [`ChatRequest`] it receives so tests can inspect the full
/// conversation that would have been sent to a real model API.
///
/// # Panics
///
/// Panics if called more times than there are queued responses.
#[derive(Clone)]
pub struct MockChatClient {
    /// Queued responses returned in order.
    responses: Arc<Mutex<Vec<ModelResponse>>>,
    /// All requests received, in order.
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}

impl MockChatClient {
    /// Create a client with a sequence of canned responses.
    ///
    /// Responses are returned in order: the first call returns `responses[0]`,
    /// the second call returns `responses[1]`, and so on.
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
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
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(RhoError::Unexpected(anyhow::anyhow!(
                "MockChatClient: no more canned responses"
            )));
        }
        Ok(responses.remove(0))
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

// ── Sandbox helpers ───────────────────────────────────────────────────────────

/// Create a temporary directory and a [`SandboxRoot`] rooted at it.
///
/// The [`TempDir`] must be kept alive for the duration of the test; it is
/// deleted when dropped.
///
/// # Panics
///
/// Panics if the temp directory or sandbox root cannot be created.
pub fn tempdir_with_sandbox() -> (TempDir, SandboxRoot) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = SandboxRoot::new(dir.path()).expect("create sandbox root");
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
