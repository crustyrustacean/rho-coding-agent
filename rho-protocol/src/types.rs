//! Typed wire-format types for the JSON-RPC 2.0 protocol.
//!
//! These structs describe the exact shape of every JSON-RPC method param,
//! method result, and notification param on the wire. They are **not** the
//! kernel types — they are the thin translation layer between the agent core
//! and the JSON-RPC transport.
//!
//! Every struct derives `Serialize` and (where applicable) `Deserialize` so
//! that:
//! - Inbound params are deserialized via `serde_json::from_value` at the
//!   dispatch layer (catches malformed requests early).
//! - Outbound results and notifications are serialized via `serde_json::to_value`
//!   (replaces hand-built `json!({...})` calls).
//!
//! Field names use `#[serde(rename_all = "camelCase")]` to match the JSON-RPC
//! wire format while keeping Rust code idiomatic (`snake_case`).

#![allow(clippy::missing_docs_in_private_items)]

use serde::{Deserialize, Serialize};

// ── Method params ──────────────────────────────────────────────────────────────

/// Params for the `prompt` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams {
    /// The user's message text.
    pub message: String,
    /// Mid-turn steering nudge.
    ///
    /// When `true`, the message is queued and injected at the next tool-batch
    /// seam (just before the following LLM call) instead of starting a new
    /// turn — matching pi's `streamingBehavior: "steer"`. Defaults to `false`
    /// so existing `prompt` payloads are unaffected.
    #[serde(default)]
    pub steer: bool,
}

/// Params for the `setModel` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelParams {
    pub model: String,
}

/// Params for the `resumeSession` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionParams {
    pub path: String,
}

/// Params for the `approvalResponse` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ApprovalResponseParams {
    pub approved: bool,
    /// Optional redirect message. When `approved` is `false` and this is
    /// present, the agent treats it as a redirect — the user's alternative
    /// instructions are injected as a conversation turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ── Method results ────────────────────────────────────────────────────────────

/// Result for `prompt` — the agent's reply text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub reply: String,
}

/// Result for `prompt` on error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptErrorResult {
    pub error: String,
}

/// Result for `getState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetStateResult {
    pub model: String,
    pub provider: String,
    pub cwd: String,
    /// Number of conversation messages on the active path.
    ///
    /// Duplicates `getSessionStats.messageCount` so a frontend can render a
    /// banner with one round trip. Additive (`#[serde(default)]`); streaming
    /// state is NOT here — it's event-driven (`agent/start` + `agent/end`),
    /// since `getState` is processed serially and can't observe a turn
    /// mid-flight.
    #[serde(default)]
    pub message_count: u64,
}

/// Result for `setModel`.
///
/// Beyond the resolved model/provider, carries the post-switch context-window
/// stats so clients can refresh their footer in the same round-trip (no
/// separate `getSessionStats` needed just to reflect the new model's window).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelResult {
    pub model: String,
    pub provider: String,
    pub context_window: u64,
    pub estimated_used: u64,
    pub utilization_percent: u64,
}

/// Result for `getSessionStats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSessionStatsResult {
    pub context_window: u64,
    pub completion_reserve: u64,
    pub estimated_used: u64,
    pub estimated_remaining: u64,
    pub utilization_percent: u64,
    pub message_count: u64,
    pub entry_count: u64,
    pub path_entry_count: u64,
    pub compacted_entry_count: u64,
    pub compaction_tokens: u64,
    pub role_tokens: RoleTokenDist,
    pub resolution_tokens: ResolutionTokenDist,
    pub phase_tokens: PhaseTokenDistWire,
    pub api_usage: ApiUsageWire,
}

/// Wire-format role token distribution (nested inside `getSessionStats`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleTokenDist {
    pub system: u64,
    pub user: u64,
    pub assistant: u64,
    pub tool: u64,
}

/// Wire-format resolution token distribution (nested inside `getSessionStats`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionTokenDist {
    pub full: u64,
    pub outlined: u64,
    pub summarized: u64,
    pub pinned: u64,
}

/// Wire-format phase token distribution (nested inside `getSessionStats`).
///
/// Shows how the live context is distributed across session phases, which
/// tracks the shape of the work being done (exploration vs execution vs
/// verification vs conclusion). Only meaningful once the session has
/// classified entries into phases; otherwise all buckets are zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseTokenDistWire {
    pub exploration: u64,
    pub execution: u64,
    pub verification: u64,
    pub conclusion: u64,
    pub unclassified: u64,
}

/// Wire-format API usage (nested inside `getSessionStats`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiUsageWire {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub request_count: u32,
}

/// Result for `listModels`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListModelsResult {
    pub models: Vec<ModelEntry>,
}

/// A single model entry in `listModels`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
}

/// Result for `listProviders`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProvidersResult {
    pub providers: Vec<ProviderEntry>,
}

/// A single provider entry in `listProviders`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub name: String,
    #[serde(rename = "isExternal")]
    pub is_external: bool,
    pub reachable: bool,
    pub active: bool,
}

/// Result for `listSessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResult {
    pub sessions: Vec<SessionEntry>,
}

/// A single session entry in `listSessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub path: String,
    pub mtime_secs: u64,
    pub size_kb: u64,
    pub entry_count: u64,
}

/// Result for `listExtensions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListExtensionsResult {
    pub extensions: Vec<ExtensionEntry>,
}

/// A single extension entry in `listExtensions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionEntry {
    pub name: String,
    pub tools: Vec<String>,
}

/// Result for `listTools`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    pub tools: Vec<ToolEntry>,
}

/// A single tool entry in `listTools`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub risk: String,
    pub parameters: serde_json::Value,
}

/// Result for `resumeSession`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionResult {
    pub path: String,
    pub model: String,
    pub cwd: String,
    pub entry_count: u64,
}

/// Result for `newSession`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    /// The new session's unique id.
    pub session_id: String,
    /// Path to the new session's JSONL file (empty for in-memory sessions).
    pub path: String,
}

/// Result for `getMessages`. Messages are pre-serialized `Value`s because
/// the `ChatMessage` → JSON conversion has complex per-role shapes that
/// don't map cleanly to a single struct without coupling `rho-core` to the
/// wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMessagesResult {
    pub messages: Vec<serde_json::Value>,
}

/// Empty result for methods that return no data (`abort`, `clear`, `compact`,
/// `reloadExtensions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyResult {}

// ── Notification params ──────────────────────────────────────────────────────

/// Params for the `agent/start` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStartParams {}

/// Wire-format token usage (delta for a single `run_loop` call).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageWire {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub request_count: u32,
}

/// Wire-format tool call outcome.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallOutcomeWire {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Wire-format tool call record.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecordWire {
    pub name: String,
    pub arguments: String,
    pub outcome: ToolCallOutcomeWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Params for the `agent/end` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEndParams {
    pub reply: String,
    pub iterations: u32,
    pub usage: TokenUsageWire,
    pub tool_calls: Vec<ToolCallRecordWire>,
    pub duration_ms: u64,
    pub finish_reason: String,
}

/// Params for the `agent/error` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentErrorParams {
    pub error: String,
}

/// Params for the `state/change` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateChangeParams {
    pub state: String,
}

/// Params for the `message/delta` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDeltaParams {
    pub delta: String,
}

/// Params for the `reasoning/delta` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningDeltaParams {
    pub delta: String,
}

/// Params for the `tool/call` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub name: String,
    pub arguments: String,
}

/// Params for the `tool/result` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultParams {
    pub name: String,
    #[serde(rename = "is_error")]
    pub is_error: bool,
    pub output: String,
}

/// Params for the `tool/denied` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeniedParams {
    pub name: String,
}

/// Params for the `approval/request` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestParams {
    pub tool: String,
    pub arguments: String,
    pub risk: String,
}

/// Params for the `ready` notification.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyParams {}

/// Wire-format per-iteration usage delta (nested inside the `usage` notification).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDeltaWire {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub request_count: u32,
}

/// Wire-format live context snapshot (nested inside the `usage` notification).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageContextWire {
    pub estimated_used: u64,
    pub context_window: u64,
    pub completion_reserve: u64,
    pub utilization_percent: u8,
}

/// Params for the `usage` notification, emitted after each model response.
///
/// Carries the per-iteration token/cost delta and a live context snapshot so
/// frontends can render a context/cost gauge during long multi-iteration turns
/// without polling `getSessionStats`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageParams {
    pub iteration: u32,
    pub usage: UsageDeltaWire,
    pub context: UsageContextWire,
}

// ── Notification builder ──────────────────────────────────────────────────────

use serde_json::json;

/// Build a JSON-RPC notification from a typed params struct.
///
/// This replaces the hand-built `json!({"method": ..., "params": {...}})` calls
/// in `RpcObserver` and `RpcApprovalGate`.
pub fn notification<P: Serialize>(method: &str, params: &P) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}
