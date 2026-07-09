//! Compaction strategies for adaptive-resolution context management.
//!
//! When the session tree grows beyond the token budget, older entries can be
//! *compacted* — summarised into a [`CompactionSummary`] and transitioned from
//! [`Full`](super::entry::EntryResolution::Full) to
//! [`Compacted`](super::entry::EntryResolution::Compacted) resolution. The original entries
//! remain in the tree (accessible via [`Session::entry`]), but are bypassed by
//! the path messages builder, which instead renders the summary as
//! a synthetic `User` message.
//!
//! # Strategy trait
//!
//! [`CompactionStrategy`] is the extension point. Phase 2.5 ships
//! [`MechanicalCompactionStrategy`], which produces deterministic, structured
//! summaries without any LLM calls. Phase 4+ may introduce
//! `LlmCompactionStrategy` that populates the `notes` field with model-generated
//! prose.
//!
//! # Compaction vs. deletion
//!
//! Compaction is a *refinement* operation, not a deletion. The compacted entries
//! stay in the tree at lower resolution. This is the core of the adaptive-
//! resolution framing: keep fine detail where it matters, coarsen where it
//! doesn't.
//!
//! [`Session::entry`]: crate::session::Session::entry

use crate::error::Result;
use crate::message::ChatMessage;
use crate::newtypes::ToolName;
use crate::session::entry::{CompactionPhase, CompactionSummary, Entry, EntryPayload};
use crate::session::phase::SessionPhase;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tracing::{debug, warn};

// ── CompactionStrategy trait ──────────────────────────────────────────────────

/// Strategy for producing a structured summary of a range of session entries.
///
/// Implementations range from deterministic mechanical summarisation (no LLM
/// calls) to model-driven summarisation. The trait is `Send + Sync` so it can
/// be shared across async tasks.
///
/// # Contract
///
/// - The input entries are in **chronological order** (oldest first).
/// - The returned [`CompactionSummary`] must accurately reflect the input range.
/// - [`MechanicalCompactionStrategy`] is the default and produces deterministic
///   output for a fixed input — no randomness, no LLM calls.
#[async_trait::async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Produce a structured summary of the given entries.
    ///
    /// The entries are guaranteed to be in chronological order and all have
    /// resolution [`Full`](crate::session::EntryResolution::Full) — the caller
    /// filters before passing them.
    async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary>;
}

// ── MechanicalCompactionStrategy ──────────────────────────────────────────────

/// A deterministic compaction strategy that produces structured summaries
/// without any LLM calls.
///
/// Walks the entries and extracts:
/// - `original_request`: the first non-empty `ChatMessage::User` text (the
///   initial request that began the compacted segment — background).
/// - `current_request`: the last non-empty `ChatMessage::User` text (the
///   active task at the end of the compacted segment — what the agent should
///   resume). See [`CompactionSummary::current_request`].
/// - `tool_calls`: groups `ChatMessage::Assistant { tool_calls }` entries by
///   tool name; for each call, formats a one-line argument summary.
/// - `tokens_compacted`: sum of estimated tokens across all compacted entries.
/// - `entry_count`, `time_span`: trivial walks.
/// - `notes`: always `None` (LLM strategies populate this).
///
/// The output is deterministic for a fixed input — no randomness, no external
/// calls. This makes it suitable for testing and for cases where model
/// summarisation is unavailable or too expensive.
#[derive(Default)]
pub struct MechanicalCompactionStrategy {
    /// Token estimator for computing `tokens_compacted`.
    /// Uses a simple chars/4 heuristic; the session's calibrated estimator
    /// is not needed here because compaction is an approximate operation.
    _phantom: (),
}

impl MechanicalCompactionStrategy {
    /// Create a new mechanical compaction strategy.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl CompactionStrategy for MechanicalCompactionStrategy {
    async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary> {
        let mut original_request: Option<String> = None;
        let mut current_request: Option<String> = None;
        let mut tool_calls: BTreeMap<ToolName, Vec<String>> = BTreeMap::new();
        let mut tokens_compacted: usize = 0;
        let mut first_timestamp: Option<std::time::SystemTime> = None;
        let mut last_timestamp: Option<std::time::SystemTime> = None;
        let mut key_findings: BTreeMap<ToolName, Vec<String>> = BTreeMap::new();
        let mut tool_call_names: HashMap<String, ToolName> = HashMap::new();

        // Phase-aware tracking: we replay the phase state machine across
        // entries and group tool activity into phase segments.
        let mut phase_tracker = PhaseTracker::new();

        for entry in entries {
            track_time_bounds(entry, &mut first_timestamp, &mut last_timestamp);
            tokens_compacted += estimate_entry_tokens(entry);
            process_entry(
                entry,
                &mut original_request,
                &mut current_request,
                &mut tool_calls,
                &mut key_findings,
                &mut tool_call_names,
                &mut phase_tracker,
            );
        }

        // Flush any remaining segment
        phase_tracker.flush();

        let time_span = match (first_timestamp, last_timestamp) {
            (Some(first), Some(last)) => last.duration_since(first).unwrap_or(Duration::ZERO),
            _ => Duration::ZERO,
        };

        Ok(CompactionSummary {
            original_request,
            current_request,
            tool_calls,
            tokens_compacted,
            entry_count: entries.len(),
            time_span,
            notes: None,
            key_findings,
            phases: phase_tracker.into_phases(),
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Flush a non-empty phase segment into the phases vector.
///
/// A segment is only flushed if it contains any tool activity (calls or
/// findings). Empty segments (e.g., user messages before any tool calls)
/// are silently discarded to avoid cluttering the summary with empty phases.
fn flush_phase_segment(segment: &mut CompactionPhase, phases: &mut Vec<CompactionPhase>) {
    let has_content = !segment.tool_calls.is_empty() || !segment.key_findings.is_empty();
    if has_content {
        let flushed = std::mem::take(segment);
        phases.push(flushed);
    }
}

/// Tracks session phase transitions across compacted entries and accumulates
/// tool activity into phase segments.
struct PhaseTracker {
    /// Current phase being tracked.
    current_phase: SessionPhase,
    /// Whether any edit/write tools have been executed.
    has_had_edits: bool,
    /// Accumulator for the current phase segment.
    current_segment: CompactionPhase,
    /// Completed phase segments.
    phases: Vec<CompactionPhase>,
}

impl PhaseTracker {
    /// Create a new phase tracker starting in Exploration.
    fn new() -> Self {
        Self {
            current_phase: SessionPhase::Exploration,
            has_had_edits: false,
            current_segment: CompactionPhase::default(),
            phases: Vec::new(),
        }
    }

    /// Flush the current segment into the phases list.
    fn flush(&mut self) {
        flush_phase_segment(&mut self.current_segment, &mut self.phases);
    }

    /// Prepend phases from a prior compaction summary being rolled forward.
    ///
    /// A prior summary represents older work than anything accumulated so far
    /// (on a single leaf path only the latest compaction is Full-resolution;
    /// earlier ones are `Compacted` and filtered out before compaction runs),
    /// so its phases belong at the front of the list. The caller should
    /// [`flush`](Self::flush) first to close any in-progress segment.
    fn absorb_prior_phases(&mut self, phases: Vec<CompactionPhase>) {
        let mut combined = phases;
        combined.append(&mut self.phases);
        self.phases = combined;
    }

    /// Consume the tracker and return the completed phase segments.
    fn into_phases(self) -> Vec<CompactionPhase> {
        self.phases
    }

    /// Handle a tool call: transition phase and ensure the current
    /// segment matches.
    fn on_tool_call(&mut self, tool_name: &str) {
        self.current_phase = crate::session::phase::transition_phase(
            self.current_phase,
            tool_name,
            self.has_had_edits,
        );
        if matches!(self.current_phase, SessionPhase::Execution) {
            self.has_had_edits = true;
        }
        let phase_str = self.current_phase.as_str().to_owned();
        if self.current_segment.phase != phase_str {
            flush_phase_segment(&mut self.current_segment, &mut self.phases);
            self.current_segment.phase = phase_str;
        }
    }
}

/// Track time bounds across entries.
fn track_time_bounds(
    entry: &Entry,
    first: &mut Option<std::time::SystemTime>,
    last: &mut Option<std::time::SystemTime>,
) {
    if first.is_none() || entry.timestamp < first.unwrap() {
        *first = Some(entry.timestamp);
    }
    if last.is_none() || entry.timestamp > last.unwrap() {
        *last = Some(entry.timestamp);
    }
}

/// Process a single entry for compaction.
fn process_entry(
    entry: &Entry,
    original_request: &mut Option<String>,
    current_request: &mut Option<String>,
    tool_calls: &mut BTreeMap<ToolName, Vec<String>>,
    key_findings: &mut BTreeMap<ToolName, Vec<String>>,
    tool_call_names: &mut HashMap<String, ToolName>,
    phase_tracker: &mut PhaseTracker,
) {
    match &entry.payload {
        EntryPayload::Message(msg) => match msg {
            ChatMessage::User { content } => {
                phase_tracker.flush();
                let text = extract_text(content);
                if !text.is_empty() {
                    // First user message in the range is the initial request
                    // (background on how the segment began).
                    if original_request.is_none() {
                        *original_request = Some(text.clone());
                    }
                    // Overwrite every turn so we end on the LAST user message —
                    // the active task the agent should resume after compaction.
                    *current_request = Some(text.clone());
                    phase_tracker.current_segment.user_messages.push(text);
                }
            }
            ChatMessage::Assistant {
                tool_calls: calls, ..
            } => {
                for call in calls {
                    tool_calls
                        .entry(call.function.name.clone())
                        .or_default()
                        .push(summarise_arguments(&call.function.arguments));
                    tool_call_names.insert(call.id.to_string(), call.function.name.clone());

                    phase_tracker.on_tool_call(&call.function.name);

                    phase_tracker
                        .current_segment
                        .tool_calls
                        .entry(call.function.name.clone())
                        .or_default()
                        .push(summarise_arguments(&call.function.arguments));
                }
            }
            ChatMessage::System { .. } => {}
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                if let Some(name) = tool_call_names.get(&tool_call_id.to_string()) {
                    let text = extract_text(content);
                    if !text.is_empty() {
                        key_findings
                            .entry(name.clone())
                            .or_default()
                            .push(summarise_result(&text));

                        phase_tracker
                            .current_segment
                            .key_findings
                            .entry(name.clone())
                            .or_default()
                            .push(summarise_result(&text));
                    }
                }
            }
        },
        EntryPayload::Compaction { summary, .. } => {
            // A prior compaction summary is being rolled forward. On a single
            // leaf path only the *latest* compaction is Full-resolution
            // (earlier ones are `Compacted` and filtered out before reaching
            // here), so this summary represents older work than anything
            // accumulated so far. Preserve it instead of silently dropping it:
            //   - inherit its request as the earliest-known `original_request`,
            //   - its `current_request` supersedes earlier user messages (it
            //     is newer), and is itself superseded by any later one,
            //   - its phase-structured activity is kept at the front.
            phase_tracker.flush();
            if original_request.is_none() {
                original_request.clone_from(&summary.original_request);
            }
            if let Some(req) = &summary.current_request {
                *current_request = Some(req.clone());
            }
            // Roll the prior summary's phase-structured activity forward.
            // Modern summaries always populate `phases` for any tool activity
            // (the flat `tool_calls`/`key_findings` fields carry the same
            // data and exist only for pre-Phase-5 fallback rendering), so
            // absorbing `phases` preserves everything.
            phase_tracker.absorb_prior_phases(summary.phases.clone());
        }
        // BranchSummary and other payloads are not rolled forward.
        _ => {}
    }
}

/// Produce a one-line summary of a tool result.
///
/// Truncates to ~150 characters at a UTF-8-safe boundary.
fn summarise_result(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= 150 {
        trimmed.to_owned()
    } else {
        let mut end = 147;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

// ── LlmCompactionStrategy ─────────────────────────────────────────────────

/// Maximum length for the LLM-generated compaction notes, in characters.
/// Truncated at a UTF-8-safe boundary if exceeded.
const MAX_NOTES_CHARS: usize = 2000;

/// Maximum length for the mechanical summary fed to the LLM, in characters.
/// Truncated if exceeded to keep the compaction request small.
const MAX_MECHANICAL_FEED_CHARS: usize = 4000;

/// A compaction strategy that uses an LLM to generate narrative summaries.
///
/// This strategy runs [`MechanicalCompactionStrategy`] first to produce the
/// structured fields (`original_request`, `tool_calls`, `key_findings`, `phases`,
/// etc.), then calls the LLM to produce a natural-language narrative that
/// populates the `notes` field of [`CompactionSummary`].
///
/// The two-stage approach means:
/// - Structured fields are always deterministic (from mechanical compaction).
/// - The `notes` field adds LLM-generated context (what the agent was trying to do,
///   what decisions were made, what state was left in).
///
/// If the LLM call fails, the strategy falls back to mechanical-only compaction
/// (returns the summary with `notes = None` and logs a warning). This ensures
/// compaction never blocks the agent loop.
pub struct LlmCompactionStrategy {
    /// The LLM service to call for summarisation.
    client: std::sync::Arc<dyn rho_ai::LlmService>,
    /// The model identifier to use for the compaction request.
    model: String,
    /// Maximum tokens for the LLM's completion response.
    max_tokens: usize,
    /// The underlying mechanical strategy (always runs first).
    mechanical: MechanicalCompactionStrategy,
}

impl LlmCompactionStrategy {
    /// Create a new LLM compaction strategy.
    ///
    /// # Arguments
    ///
    /// * `client` — The LLM service to call for summarisation.
    /// * `model` — The model identifier (e.g. `"gpt-4o"`, `"qwen3-8b"`).
    pub fn new(client: std::sync::Arc<dyn rho_ai::LlmService>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            max_tokens: 512,
            mechanical: MechanicalCompactionStrategy::new(),
        }
    }

    /// Set the maximum tokens for the LLM's completion response.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Build the compaction prompt from a mechanical summary.
    ///
    /// Renders the structured summary into a concise prompt that asks the LLM
    /// to produce a narrative summary of what happened during the compacted
    /// session segment.
    fn build_prompt(summary: &CompactionSummary) -> String {
        use std::fmt::Write;

        let mut prompt = String::new();

        prompt.push_str(
            "You are a session summariser. Given the following structured summary of a \
             coding session segment, write a concise narrative (2–4 sentences) describing \
             what the user asked for, what the agent did, and the current state. Focus on \
             information that would help the agent resume work if context is lost. Do NOT \
             repeat tool names or argument details — the structured summary already has those. \
             Output ONLY the narrative, no preamble.\n\n",
        );

        if let Some(ref req) = summary.current_request {
            let _ = writeln!(prompt, "Current request: {req}");
        }
        if let Some(ref req) = summary.original_request {
            let _ = writeln!(prompt, "Original request: {req}");
        }

        // Render the phase-structured or flat summary
        if summary.phases.is_empty() {
            if !summary.tool_calls.is_empty() {
                prompt.push_str("Tool activity:\n");
                for (name, calls) in &summary.tool_calls {
                    let _ = writeln!(prompt, "  {name}: {} calls", calls.len());
                }
            }
        } else {
            prompt.push_str("Activity by phase:\n");
            for segment in &summary.phases {
                let _ = write!(
                    prompt,
                    "  {}: {} tool calls",
                    segment.phase,
                    segment.tool_calls.values().map(Vec::len).sum::<usize>()
                );
                if !segment.key_findings.is_empty() {
                    prompt.push_str(", key findings available");
                }
                if !segment.user_messages.is_empty() {
                    prompt.push_str(", user context available");
                }
                prompt.push('\n');
            }
        }

        let _ = write!(
            prompt,
            "Entries compacted: {}\nTokens compacted: {}\n",
            summary.entry_count, summary.tokens_compacted
        );

        // Truncate if too long
        if prompt.len() > MAX_MECHANICAL_FEED_CHARS {
            let mut end = MAX_MECHANICAL_FEED_CHARS;
            while !prompt.is_char_boundary(end) {
                end -= 1;
            }
            prompt.truncate(end);
            prompt.push_str("\n[truncated]");
        }

        prompt
    }

    /// Call the LLM and extract the text response.
    async fn call_llm(&self, prompt: &str) -> std::result::Result<String, String> {
        use futures::StreamExt;
        use rho_ai::StreamEvent;
        use rho_ai::types::{LlmMessage, LlmRequest};

        let request = LlmRequest::new(
            self.model.clone(),
            vec![LlmMessage::User(prompt.to_owned())],
        )
        .with_max_tokens(self.max_tokens);

        let stream = self
            .client
            .chat_stream(request)
            .await
            .map_err(|e| format!("LLM call failed: {e}"))?;

        let events: Vec<StreamEvent> = stream
            .collect::<Vec<std::result::Result<StreamEvent, _>>>()
            .await
            .into_iter()
            .collect::<std::result::Result<Vec<StreamEvent>, _>>()
            .map_err(|e| format!("LLM stream error: {e}"))?;
        let response = StreamEvent::accumulate(&events);

        if !response.tool_calls.is_empty() {
            return Err("LLM returned tool calls instead of text".to_owned());
        }

        Ok(response.text)
    }

    /// Truncate notes to the maximum length at a UTF-8-safe boundary.
    fn truncate_notes(text: &str) -> String {
        if text.len() <= MAX_NOTES_CHARS {
            return text.to_owned();
        }
        let mut end = MAX_NOTES_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    }
}

#[async_trait::async_trait]
impl CompactionStrategy for LlmCompactionStrategy {
    async fn compact(&self, entries: &[&Entry]) -> Result<CompactionSummary> {
        // Stage 1: Run mechanical compaction for structured fields.
        let mut summary = self.mechanical.compact(entries).await?;

        if entries.is_empty() {
            return Ok(summary);
        }

        // Stage 2: Call the LLM for narrative notes.
        let prompt = Self::build_prompt(&summary);
        match self.call_llm(&prompt).await {
            Ok(notes) if !notes.trim().is_empty() => {
                let trimmed = Self::truncate_notes(notes.trim());
                debug!(notes_len = trimmed.len(), "LLM compaction notes generated");
                summary.notes = Some(trimmed);
            }
            Ok(_) => {
                debug!("LLM returned empty notes, using mechanical-only summary");
            }
            Err(e) => {
                warn!(error = %e, "LLM compaction failed, using mechanical-only summary");
            }
        }

        Ok(summary)
    }
}

/// Estimate the token count for an entry using the chars/4 heuristic.
///
/// This is deliberately simple — compaction doesn't need calibrated estimates.
/// The session's calibrated estimator is used for budget decisions (`fit_path`),
/// not for compaction summaries.
fn estimate_entry_tokens(entry: &Entry) -> usize {
    let chars = match &entry.payload {
        EntryPayload::Message(msg) => message_chars(msg),
        EntryPayload::Compaction { summary, .. } => {
            // Size a compaction entry by its rendered summary text only.
            // Re-charging `tokens_before` (the historic count absorbed by the
            // *prior* compaction) makes the entry permanently "heavy": every
            // successive compaction re-embeds the full cumulative history, so
            // `tokens_compacted` grows monotonically and the summary balloons
            // (observed in the field: 17 compactions, last summary ~250k
            // tokens). Mirrors `estimate_entry_tokens_for_compaction` in
            // `truncation.rs`, which sizes by the summary alone.
            summary_text_chars(summary)
        }
        EntryPayload::BranchSummary { summary, .. } => summary_text_chars(summary),
        EntryPayload::Custom { data, .. } => data.to_string().len(),
        EntryPayload::CustomMessage { content, .. } => {
            content.iter().map(block_chars).sum::<usize>() + 20
        }
        EntryPayload::ModelChange { model } => model.len() + 20,
        EntryPayload::Label { label, .. } => label.as_ref().map_or(20, |l| l.len() + 20),
        EntryPayload::SessionInfo { name } => name.len() + 20,
        EntryPayload::LeafMoved { .. } => 40,
        EntryPayload::SessionEnded { .. } => 1,
    };

    chars.div_ceil(4).max(1)
}

/// Count the approximate character content of a `ChatMessage`.
fn message_chars(msg: &ChatMessage) -> usize {
    use crate::message::ContentBlock;

    let mut chars = 20; // structural overhead
    match msg {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. } => {
            for block in content {
                chars += match block {
                    ContentBlock::Text { text } => text.len(),
                };
            }
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            chars += tool_call_id.len() + 10;
            for block in content {
                chars += match block {
                    ContentBlock::Text { text } => text.len(),
                };
            }
        }
    }
    chars
}

/// Count the character content of a `ContentBlock`.
fn block_chars(block: &crate::message::ContentBlock) -> usize {
    match block {
        crate::message::ContentBlock::Text { text } => text.len(),
    }
}

/// Count the approximate character content of a `CompactionSummary`.
fn summary_text_chars(summary: &CompactionSummary) -> usize {
    let mut chars = 100; // base overhead for header, etc.
    if let Some(ref req) = summary.original_request {
        chars += req.len();
    }
    if let Some(ref req) = summary.current_request {
        chars += req.len();
    }
    // Mirror `render_compaction_summary`: phases when present, else flat.
    if summary.phases.is_empty() {
        for (name, calls) in &summary.tool_calls {
            chars += name.len() + calls.iter().map(|c| c.len() + 2).sum::<usize>();
        }
        for findings in summary.key_findings.values() {
            chars += findings.iter().map(|f| f.len() + 2).sum::<usize>();
        }
    } else {
        for segment in &summary.phases {
            chars += segment.phase.len() + 10; // phase header overhead
            for (name, calls) in &segment.tool_calls {
                chars += name.len() + calls.iter().map(|c| c.len() + 2).sum::<usize>();
            }
            for findings in segment.key_findings.values() {
                chars += findings.iter().map(|f| f.len() + 2).sum::<usize>();
            }
        }
    }
    if let Some(ref notes) = summary.notes {
        chars += notes.len();
    }
    chars
}

/// Extract the plain text from a slice of `ContentBlock`s.
fn extract_text(content: &[crate::message::ContentBlock]) -> String {
    content
        .iter()
        .map(|b| match b {
            crate::message::ContentBlock::Text { text } => text.as_str(),
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Produce a one-line argument summary from a JSON arguments string.
///
/// Tries to extract short, human-readable values from the JSON. For simple
/// string arguments, shows the value. For complex objects, shows a truncated
/// representation.
fn summarise_arguments(arguments: &str) -> String {
    // Try to parse as JSON and extract a short summary
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(arguments) {
        match &val {
            serde_json::Value::Object(map) => {
                // Show key=value pairs, truncated
                let pairs: Vec<String> = map
                    .iter()
                    .take(3) // at most 3 key-value pairs
                    .map(|(k, v)| {
                        let v_str = match v {
                            serde_json::Value::String(s) => {
                                if s.len() > 40 {
                                    let mut end = 38;
                                    while !s.is_char_boundary(end) {
                                        end -= 1;
                                    }
                                    format!("{}…", &s[..end])
                                } else {
                                    s.clone()
                                }
                            }
                            other => {
                                let s = other.to_string();
                                if s.len() > 40 {
                                    let mut end = 38;
                                    while !s.is_char_boundary(end) {
                                        end -= 1;
                                    }
                                    format!("{}…", &s[..end])
                                } else {
                                    s
                                }
                            }
                        };
                        format!("{k}={v_str}")
                    })
                    .collect();
                let summary = pairs.join(", ");
                if map.len() > 3 {
                    format!("{summary}, …")
                } else {
                    summary
                }
            }
            other => {
                let s = other.to_string();
                if s.len() > 60 {
                    let mut end = 58;
                    while !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}…", &s[..end])
                } else {
                    s
                }
            }
        }
    } else {
        // Not valid JSON — truncate the raw string
        if arguments.len() > 60 {
            let mut end = 58;
            while !arguments.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &arguments[..end])
        } else {
            arguments.to_owned()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    use super::*;
    use crate::message::{ChatMessage, ModelToolCall, ToolCallFunction};
    use crate::newtypes::{EntryId, ToolCallId, ToolName};
    use crate::session::entry::{Entry, EntryPayload, EntryResolution};
    use std::time::{Duration, SystemTime};

    /// Helper: create a test entry with the given payload and Full resolution.
    fn full_entry(payload: EntryPayload) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    /// Helper: create a test entry with a specific timestamp.
    fn timed_entry(payload: EntryPayload, ts: SystemTime) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: ts,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    // ── MechanicalCompactionStrategy tests ─────────────────────────────────

    #[tokio::test]
    async fn mechanical_extracts_original_request() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "fix the bug in main.rs",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text(
                "looking at it",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(
            summary.original_request,
            Some("fix the bug in main.rs".to_owned())
        );
        // With a single user message, both initial and current capture it.
        assert_eq!(
            summary.current_request,
            Some("fix the bug in main.rs".to_owned())
        );
    }

    #[tokio::test]
    async fn mechanical_original_request_is_first_user_message() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("hi"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("first"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("second"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        // The FIRST user message is the initial (background) request…
        assert_eq!(summary.original_request, Some("first".to_owned()));
        // …while the MOST RECENT user message is the active task to resume.
        assert_eq!(summary.current_request, Some("second".to_owned()));
    }

    #[tokio::test]
    async fn mechanical_no_user_message_gives_none_original_request() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("hi"))),
            full_entry(EntryPayload::Message(ChatMessage::system_text("sys"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.original_request, None);
        assert_eq!(summary.current_request, None);
    }

    /// Regression test for the "rho lost itself" compaction bug.
    ///
    /// In a long, multi-topic session the first user message can be hours old
    /// and about a totally different topic than the current task. Compaction
    /// must anchor the post-compaction context to the **most recent** user
    /// request (the active task), not the session-opening message — otherwise
    /// the model resumes the stale request instead of the current one.
    ///
    /// Mirrors the incident recorded in session `1783487802`: the opening
    /// request was "What documents are in your memory?" while the active task
    /// was a question about `RpcApprovalGate`. After compaction the model
    /// reverted to listing memory documents. With `current_request`, the
    /// rendered summary now leads with the active task and demotes the stale
    /// opening request to background context.
    #[tokio::test]
    async fn mechanical_current_request_is_most_recent_not_first() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "What documents are in your memory for this project?",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text(
                "Here are the 4 documents stored in project memory…",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "Do we need a new constructor, `new()` method on `RpcApprovalGate`?",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text(
                "Yes — a `new()` makes the oneshot wiring explicit.",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "Why `Arc<Mutex<…` for our oneshot channel?",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = MechanicalCompactionStrategy::new()
            .compact(&refs)
            .await
            .unwrap();

        // The initial request is the (now stale) session-opening message…
        assert_eq!(
            summary.original_request,
            Some("What documents are in your memory for this project?".to_owned())
        );
        // …but the active task is the most-recent user message.
        assert_eq!(
            summary.current_request,
            Some("Why `Arc<Mutex<…` for our oneshot channel?".to_owned())
        );

        // The rendered summary must lead with the current task so the model
        // resumes the right work, and demote the stale opening request.
        let msg = crate::context::render_compaction_summary(&summary);
        let ChatMessage::User { content } = &msg else {
            panic!("expected synthetic User message");
        };
        let crate::message::ContentBlock::Text { text } = &content[0];
        assert!(
            text.contains("Current request: \"Why `Arc<Mutex<…` for our oneshot channel?\""),
            "rendered summary must lead with the current task, got: {text}"
        );
        assert!(
            text.contains(
                "Initial request: \"What documents are in your memory for this project?\""
            ),
            "stale opening request should appear as background, got: {text}"
        );
        let cur = text.find("Current request:").unwrap();
        let init = text.find("Initial request:").unwrap();
        assert!(
            cur < init,
            "current request must precede the initial request"
        );
    }

    /// Regression test for the re-compaction fidelity gap.
    ///
    /// When a compaction range includes a *prior* `Compaction` entry, the
    /// prior summary's content (request + phase-structured activity) must be
    /// rolled forward into the new summary, not silently dropped. Previously
    /// `process_entry` only handled `Message` payloads, so a rolled-up prior
    /// summary vanished — losing the record of earlier work.
    #[tokio::test]
    async fn mechanical_compaction_preserves_prior_summary() {
        let prior_summary = CompactionSummary {
            original_request: Some("the original goal".to_owned()),
            current_request: Some("rolled-forward task".to_owned()),
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases: vec![CompactionPhase {
                phase: "exploration".to_owned(),
                tool_calls: {
                    let mut m = BTreeMap::new();
                    m.insert(ToolName::from("read_file"), vec!["src/main.rs".to_owned()]);
                    m
                },
                key_findings: {
                    let mut m = BTreeMap::new();
                    m.insert(
                        ToolName::from("read_file"),
                        vec!["found the bug".to_owned()],
                    );
                    m
                },
                user_messages: vec![],
            }],
            tokens_compacted: 5_000,
            entry_count: 10,
            time_span: Duration::from_mins(1),
            notes: None,
        };
        let entries = [
            full_entry(EntryPayload::Compaction {
                summary: prior_summary,
                first_kept: EntryId::new(),
                tokens_before: 5_000,
            }),
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "and now a new request",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("c1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("run_command"),
                        arguments: r#"{\"command\":\"cargo test\"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = MechanicalCompactionStrategy::new()
            .compact(&refs)
            .await
            .unwrap();

        // original_request inherited from the prior summary (earliest).
        assert_eq!(
            summary.original_request,
            Some("the original goal".to_owned())
        );
        // current_request superseded by the later user message.
        assert_eq!(
            summary.current_request,
            Some("and now a new request".to_owned())
        );
        // Prior phase activity is preserved (not silently dropped)...
        let has_prior = summary.phases.iter().any(|p| {
            p.phase == "exploration"
                && p.tool_calls.contains_key(&ToolName::from("read_file"))
                && p.key_findings
                    .values()
                    .flatten()
                    .any(|f| f.contains("found the bug"))
        });
        assert!(
            has_prior,
            "prior compaction phases must be preserved, got: {:#?}",
            summary.phases
        );
        // ...and the prior phase is ordered before the new activity (it is
        // older), since a prior summary represents older work.
        let prior_idx = summary
            .phases
            .iter()
            .position(|p| p.phase == "exploration")
            .unwrap();
        let new_idx = summary
            .phases
            .iter()
            .position(|p| p.tool_calls.contains_key(&ToolName::from("run_command")))
            .unwrap();
        assert!(
            prior_idx < new_idx,
            "prior phase must precede new activity ({prior_idx} < {new_idx})"
        );
    }

    #[tokio::test]
    async fn mechanical_extracts_tool_calls() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "file contents",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/lib.rs"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        // Should have one tool (read_file) with 2 calls
        assert_eq!(summary.tool_calls.len(), 1);
        let calls = &summary.tool_calls[&ToolName::from("read_file")];
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("path=src/main.rs"));
        assert!(calls[1].contains("path=src/lib.rs"));
    }

    #[tokio::test]
    async fn mechanical_groups_multiple_tools() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"a.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "a",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("run_command"),
                        arguments: r#"{"command":"cargo test"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.tool_calls.len(), 2);
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("read_file"))
        );
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("run_command"))
        );
    }

    #[tokio::test]
    async fn mechanical_counts_entries() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("a"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("b"))),
            full_entry(EntryPayload::Message(ChatMessage::user_text("c"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.entry_count, 3);
    }

    #[tokio::test]
    async fn mechanical_computes_time_span() {
        let now = SystemTime::now();
        let entries = [
            timed_entry(EntryPayload::Message(ChatMessage::user_text("a")), now),
            timed_entry(
                EntryPayload::Message(ChatMessage::assistant_text("b")),
                now + Duration::from_secs(45),
            ),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.time_span, Duration::from_secs(45));
    }

    #[tokio::test]
    async fn mechanical_empty_entries_gives_zero_summary() {
        let entries: Vec<&Entry> = vec![];

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&entries).await.unwrap();

        assert_eq!(summary.entry_count, 0);
        assert_eq!(summary.tokens_compacted, 0);
        assert_eq!(summary.original_request, None);
        assert!(summary.tool_calls.is_empty());
        assert_eq!(summary.time_span, Duration::ZERO);
        assert_eq!(summary.notes, None);
    }

    #[tokio::test]
    async fn mechanical_notes_always_none() {
        let entries = [full_entry(EntryPayload::Message(ChatMessage::user_text(
            "hello",
        )))];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert_eq!(summary.notes, None);
    }

    #[tokio::test]
    async fn mechanical_tokens_compacted_is_nonzero() {
        let entries = [full_entry(EntryPayload::Message(ChatMessage::user_text(
            "this is a reasonably long user message for token estimation",
        )))];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert!(
            summary.tokens_compacted > 0,
            "should estimate some tokens for non-empty entries"
        );
    }

    #[tokio::test]
    async fn mechanical_is_deterministic() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("fix it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"main.rs"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let s1 = strategy.compact(&refs).await.unwrap();
        let s2 = strategy.compact(&refs).await.unwrap();

        // MechanicalCompactionStrategy is deterministic for fixed input
        assert_eq!(s1.original_request, s2.original_request);
        assert_eq!(s1.tool_calls, s2.tool_calls);
        assert_eq!(s1.tokens_compacted, s2.tokens_compacted);
        assert_eq!(s1.entry_count, s2.entry_count);
        assert_eq!(s1.time_span, s2.time_span);
        assert_eq!(s1.notes, s2.notes);
    }

    #[tokio::test]
    async fn mechanical_extracts_tool_results() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("cargo_test"),
                        arguments: r"{}".to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "running 3 tests\ntest_foo ... ok\ntest_bar ... FAILED\ntest_baz ... ok\n1 failed",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert!(
            summary
                .key_findings
                .contains_key(&ToolName::from("cargo_test")),
            "key_findings should contain the tool name"
        );
        let findings = &summary.key_findings[&ToolName::from("cargo_test")];
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].contains("FAILED"),
            "finding should contain the test result"
        );
    }

    #[tokio::test]
    async fn mechanical_key_findings_groups_by_tool_name() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"a.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "contents of a.rs",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"b.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_2"),
                "contents of b.rs",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        // Both read_file results should be grouped under one tool name
        assert_eq!(summary.key_findings.len(), 1);
        let findings = &summary.key_findings[&ToolName::from("read_file")];
        assert_eq!(findings.len(), 2);
    }

    #[tokio::test]
    async fn mechanical_key_findings_empty_for_no_tool_results() {
        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("just talk"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("ok"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let strategy = MechanicalCompactionStrategy::new();
        let summary = strategy.compact(&refs).await.unwrap();

        assert!(summary.key_findings.is_empty());
    }

    // ── Argument summarisation tests ──────────────────────────────────────

    #[test]
    fn summarise_simple_string_argument() {
        let args = r#"{"path":"src/main.rs"}"#;
        let summary = summarise_arguments(args);
        assert!(summary.contains("path=src/main.rs"));
    }

    #[test]
    fn summarise_multiple_arguments() {
        let args = r#"{"path":"a.rs","offset":10}"#;
        let summary = summarise_arguments(args);
        assert!(summary.contains("path=a.rs"));
        assert!(summary.contains("offset=10"));
    }

    #[test]
    fn summarise_empty_json() {
        let args = "{}";
        let summary = summarise_arguments(args);
        // Empty object produces an empty string (no key-value pairs)
        assert!(
            summary.is_empty(),
            "empty JSON object should produce empty summary, got: {summary}"
        );
    }

    #[test]
    fn summarise_non_json_falls_back_gracefully() {
        let args = "not json at all";
        let summary = summarise_arguments(args);
        // Should not panic; just returns a truncated version
        assert!(!summary.is_empty());
    }

    #[test]
    fn summarise_truncates_long_values() {
        let long_val = "x".repeat(100);
        let args = format!(r#"{{"data":"{long_val}"}}"#);
        let summary = summarise_arguments(&args);
        // Should be truncated, not 100+ chars
        assert!(summary.len() < 80);
    }

    // ── Entry token estimation tests ──────────────────────────────────────

    #[test]
    fn estimate_tokens_for_user_message() {
        let entry = full_entry(EntryPayload::Message(ChatMessage::user_text("hello world")));
        let tokens = estimate_entry_tokens(&entry);
        assert!(tokens > 0, "should estimate tokens for a user message");
    }

    #[test]
    fn estimate_tokens_for_custom_entry() {
        let entry = full_entry(EntryPayload::Custom {
            kind: "rho.diagnostics.v1".to_owned(),
            data: serde_json::json!({"errors": 3}),
        });
        let tokens = estimate_entry_tokens(&entry);
        assert!(tokens > 0, "should estimate tokens for a custom entry");
    }

    #[test]
    fn estimate_tokens_for_compaction_entry_ignores_tokens_before() {
        // Regression: `estimate_entry_tokens` must size a `Compaction` entry
        // by its rendered summary text alone, NOT re-charge the historic
        // `tokens_before`. Re-charging made every prior compaction
        // permanently "heavy", so `tokens_compacted` grew monotonically and
        // summaries ballooned across compactions (the edge-thrash).
        let summary = CompactionSummary {
            original_request: Some("do the thing".to_owned()),
            current_request: Some("still doing it".to_owned()),
            tool_calls: BTreeMap::new(),
            tokens_compacted: 1_000_000,
            entry_count: 999,
            time_span: Duration::from_mins(1),
            notes: None,
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
        };
        let entry = full_entry(EntryPayload::Compaction {
            summary,
            first_kept: EntryId::new(),
            // A huge historic cost that must NOT inflate the estimate.
            tokens_before: 1_000_000,
        });
        let tokens = estimate_entry_tokens(&entry);
        // Summary text is tiny ("do the thing" + "still doing it" + overhead),
        // so the estimate must be small — well under the 250_000 the old
        // `(*tokens_before / 4).max(...)` formula would have produced.
        assert!(
            tokens < 100,
            "compaction entry estimate must reflect summary text ({tokens}), \
             not the historic tokens_before (old formula gave ~250_000)"
        );
    }

    // ── LlmCompactionStrategy tests ─────────────────────────────────────

    /// A mock LLM service that returns a canned text response.
    struct MockLlmService {
        response_text: String,
    }

    impl MockLlmService {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl rho_ai::LlmService for MockLlmService {
        async fn chat_stream(
            &self,
            _request: rho_ai::types::LlmRequest,
        ) -> std::result::Result<rho_ai::EventStream, rho_ai::ProviderError> {
            use futures::stream;
            use rho_ai::types::{StopReason, StreamEvent, StreamUsage};

            let events: Vec<StreamEvent> = vec![
                StreamEvent::Text(self.response_text.clone()),
                StreamEvent::Done {
                    reason: StopReason::EndTurn,
                    usage: StreamUsage::default(),
                },
            ];
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    /// A mock LLM service that returns an error.
    struct FailingLlmService {
        error_message: String,
    }

    impl FailingLlmService {
        fn new(msg: impl Into<String>) -> Self {
            Self {
                error_message: msg.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl rho_ai::LlmService for FailingLlmService {
        async fn chat_stream(
            &self,
            _request: rho_ai::types::LlmRequest,
        ) -> std::result::Result<rho_ai::EventStream, rho_ai::ProviderError> {
            Err(rho_ai::ProviderError::Sse {
                message: self.error_message.clone(),
            })
        }
    }

    #[tokio::test]
    async fn llm_compaction_populates_notes() {
        let client = std::sync::Arc::new(MockLlmService::new(
            "The user asked to fix a bug in main.rs. The agent read the file, \
             found the issue in the parse function, and applied a fix.",
        ));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text(
                "fix the bug in main.rs",
            ))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"src/main.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::tool_result(
                ToolCallId::from("call_1"),
                "file contents here",
            ))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = strategy.compact(&refs).await.unwrap();

        // Mechanical fields should still be populated
        assert_eq!(
            summary.original_request,
            Some("fix the bug in main.rs".to_owned())
        );
        assert_eq!(summary.entry_count, 3);

        // Notes should be populated from the LLM
        assert!(summary.notes.is_some(), "notes should be populated by LLM");
        let notes = summary.notes.unwrap();
        assert!(
            notes.contains("fix a bug"),
            "notes should contain the LLM's narrative: {notes}"
        );
    }

    #[tokio::test]
    async fn llm_compaction_includes_structured_fields_from_mechanical() {
        let client = std::sync::Arc::new(MockLlmService::new("Agent did some work."));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("do it"))),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_1"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("read_file"),
                        arguments: r#"{"path":"a.rs"}"#.to_owned(),
                    },
                }],
            })),
            full_entry(EntryPayload::Message(ChatMessage::Assistant {
                content: vec![],
                tool_calls: vec![ModelToolCall {
                    id: ToolCallId::from("call_2"),
                    call_type: "function".to_owned(),
                    function: ToolCallFunction {
                        name: ToolName::from("run_command"),
                        arguments: r#"{"command":"cargo test"}"#.to_owned(),
                    },
                }],
            })),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = strategy.compact(&refs).await.unwrap();

        // Structured fields from mechanical compaction
        assert_eq!(summary.tool_calls.len(), 2);
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("read_file"))
        );
        assert!(
            summary
                .tool_calls
                .contains_key(&ToolName::from("run_command"))
        );
        assert_eq!(summary.entry_count, 3);
        assert!(summary.tokens_compacted > 0);
    }

    #[tokio::test]
    async fn llm_compaction_falls_back_on_llm_error() {
        let client = std::sync::Arc::new(FailingLlmService::new("model overloaded"));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("hello"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("world"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        // Should NOT error — falls back to mechanical-only
        let summary = strategy.compact(&refs).await.unwrap();

        // Mechanical fields should still be populated
        assert_eq!(summary.original_request, Some("hello".to_owned()));
        assert_eq!(summary.entry_count, 2);
        // Notes should be None (LLM failed)
        assert_eq!(summary.notes, None, "notes should be None when LLM fails");
    }

    #[tokio::test]
    async fn llm_compaction_falls_back_on_empty_response() {
        let client = std::sync::Arc::new(MockLlmService::new("   "));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("hello"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("world"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = strategy.compact(&refs).await.unwrap();
        assert_eq!(
            summary.notes, None,
            "notes should be None for empty LLM response"
        );
    }

    #[tokio::test]
    async fn llm_compaction_empty_entries_returns_empty() {
        let client = std::sync::Arc::new(MockLlmService::new("won't be called"));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries: Vec<&Entry> = vec![];
        let summary = strategy.compact(&entries).await.unwrap();

        assert_eq!(summary.entry_count, 0);
        assert_eq!(summary.notes, None);
    }

    #[tokio::test]
    async fn llm_compaction_truncates_long_notes() {
        let long_response = "x".repeat(5000);
        let client = std::sync::Arc::new(MockLlmService::new(long_response));
        let strategy = LlmCompactionStrategy::new(client, "test-model");

        let entries = [
            full_entry(EntryPayload::Message(ChatMessage::user_text("hello"))),
            full_entry(EntryPayload::Message(ChatMessage::assistant_text("world"))),
        ];
        let refs: Vec<&Entry> = entries.iter().collect();

        let summary = strategy.compact(&refs).await.unwrap();

        assert!(summary.notes.is_some());
        let notes = summary.notes.unwrap();
        // 2000 bytes of ASCII + 3 bytes for '…' = 2003
        assert!(
            notes.len() <= MAX_NOTES_CHARS + 4,
            "notes should be truncated to ~{MAX_NOTES_CHARS} chars, got {}",
            notes.len()
        );
    }

    // ── build_prompt tests ────────────────────────────────────────────────

    #[test]
    fn build_prompt_includes_original_request() {
        let summary = CompactionSummary {
            original_request: Some("fix the parser bug".to_owned()),
            current_request: None,
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
            tokens_compacted: 500,
            entry_count: 10,
            time_span: Duration::from_secs(30),
            notes: None,
        };
        let prompt = LlmCompactionStrategy::build_prompt(&summary);
        assert!(
            prompt.contains("fix the parser bug"),
            "prompt should include original request"
        );
    }

    #[test]
    fn build_prompt_includes_phase_info() {
        let mut phases = Vec::new();
        let mut phase = CompactionPhase {
            phase: "exploration".to_owned(),
            ..Default::default()
        };
        phase
            .tool_calls
            .insert(ToolName::from("read_file"), vec!["main.rs".to_owned()]);
        phases.push(phase);

        let summary = CompactionSummary {
            original_request: None,
            current_request: None,
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases,
            tokens_compacted: 300,
            entry_count: 5,
            time_span: Duration::from_secs(10),
            notes: None,
        };
        let prompt = LlmCompactionStrategy::build_prompt(&summary);
        assert!(
            prompt.contains("exploration"),
            "prompt should include phase name"
        );
        assert!(
            prompt.contains("1 tool calls"),
            "prompt should include tool call count"
        );
    }

    #[test]
    fn build_prompt_includes_entry_and_token_counts() {
        let summary = CompactionSummary {
            original_request: None,
            current_request: None,
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
            tokens_compacted: 1024,
            entry_count: 15,
            time_span: Duration::from_mins(1),
            notes: None,
        };
        let prompt = LlmCompactionStrategy::build_prompt(&summary);
        assert!(
            prompt.contains("Entries compacted: 15"),
            "prompt should include entry count"
        );
        assert!(
            prompt.contains("Tokens compacted: 1024"),
            "prompt should include token count"
        );
    }

    #[test]
    fn build_prompt_truncates_long_input() {
        let summary = CompactionSummary {
            original_request: Some("x".repeat(5000)),
            current_request: None,
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
            tokens_compacted: 100,
            entry_count: 1,
            time_span: Duration::ZERO,
            notes: None,
        };
        let prompt = LlmCompactionStrategy::build_prompt(&summary);
        assert!(
            prompt.len() <= MAX_MECHANICAL_FEED_CHARS + 20,
            "prompt should be truncated to ~{MAX_MECHANICAL_FEED_CHARS} chars, got {}",
            prompt.len()
        );
        assert!(
            prompt.contains("[truncated]"),
            "truncated prompt should be marked"
        );
    }

    // ── truncate_notes tests ──────────────────────────────────────────────

    #[test]
    fn truncate_notes_short_text_unchanged() {
        let text = "short notes";
        assert_eq!(LlmCompactionStrategy::truncate_notes(text), text);
    }

    #[test]
    fn truncate_notes_long_text_truncated() {
        let text = "x".repeat(MAX_NOTES_CHARS + 100);
        let truncated = LlmCompactionStrategy::truncate_notes(&text);
        let expected_max = MAX_NOTES_CHARS + 4; // room for multi-byte ellipsis
        assert!(
            truncated.len() <= expected_max,
            "should truncate to ~{MAX_NOTES_CHARS} chars, got {}",
            truncated.len()
        );
        assert!(
            truncated.ends_with('…'),
            "truncated text should end with ellipsis"
        );
    }

    #[test]
    fn truncate_notes_at_boundary_unchanged() {
        let text = "x".repeat(MAX_NOTES_CHARS);
        let truncated = LlmCompactionStrategy::truncate_notes(&text);
        assert_eq!(truncated.len(), MAX_NOTES_CHARS);
        assert!(
            !truncated.ends_with('…'),
            "exact boundary should not add ellipsis"
        );
    }

    // ── with_max_tokens tests ────────────────────────────────────────────

    #[test]
    fn with_max_tokens_overrides_default() {
        let client = std::sync::Arc::new(MockLlmService::new("notes"));
        let strategy = LlmCompactionStrategy::new(client, "test-model").with_max_tokens(1024);
        assert_eq!(strategy.max_tokens, 1024);
    }

    #[test]
    fn default_max_tokens() {
        let client = std::sync::Arc::new(MockLlmService::new("notes"));
        let strategy = LlmCompactionStrategy::new(client, "test-model");
        assert_eq!(strategy.max_tokens, 512);
    }
}
