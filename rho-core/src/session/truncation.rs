// Truncation helpers, token estimation for tool results and compaction.

use super::entry::{CompactionSummary, Entry, EntryPayload};
use super::estimator::TokenEstimator;
use crate::message::{ChatMessage, ContentBlock};

/// Maximum fraction of the prompt budget that a single tool result may consume.
pub(crate) const MAX_TOOL_RESULT_FRACTION: f32 = 0.5;

/// Truncation footer appended to truncated tool results.
pub(crate) fn truncation_footer(original_size: usize) -> String {
    format!(
        "... [truncated; original size: {original_size} bytes — re-read the source with offset to access more]."
    )
}

/// Find the largest character boundary index ≤ `max_chars` in `s`.
///
/// Rust string slicing requires char boundaries. This finds the highest
/// index ≤ `max_chars` that falls on a valid char boundary.
pub(crate) fn floor_char_boundary(s: &str, max_chars: usize) -> usize {
    if max_chars >= s.len() {
        return s.len();
    }
    // Walk backwards from max_chars until we find a char boundary.
    let mut i = max_chars;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Estimate how many characters correspond to `target_tokens` tokens,
/// using the given estimator.
///
/// Works by solving `target_tokens = chars / ratio` for `chars`,
/// then clamping to the actual string length.
pub(crate) fn chars_to_fit_tokens(
    s: &str,
    target_tokens: usize,
    model: &str,
    estimator: &dyn TokenEstimator,
) -> usize {
    // Use the estimator's ratio: if a string of length L has T tokens,
    // then chars/token ≈ L/T, so chars ≈ target_tokens * (L/T).
    // But we can also just iterate: start from the full string and shrink.
    // For simplicity, use a proportional estimate.
    let total_tokens = estimator.estimate(model, s);
    if total_tokens == 0 {
        return s.len();
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let ratio = s.len() as f32 / total_tokens as f32;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let estimated_chars = (target_tokens as f32 * ratio) as usize;
    estimated_chars.min(s.len())
}

/// Estimate the token count for an entry, using the calibrated estimator
/// for message content.
///
/// This is used by [`Session::compact_older_than`] to decide which entries
/// to compact. It differs from the compaction module's `estimate_entry_tokens`
/// by using the calibrated estimator rather than a fixed chars/4 heuristic,
/// giving more accurate budget decisions.
pub(crate) fn estimate_entry_tokens_for_compaction(
    entry: &Entry,
    model: &str,
    estimator: &dyn TokenEstimator,
) -> usize {
    match &entry.payload {
        EntryPayload::Message(msg) => {
            let text = format_message_text(msg);
            estimator.estimate(model, &text).max(1)
        }
        EntryPayload::Compaction { summary, .. } => {
            // A compaction entry costs only what its rendered summary
            // contributes to the context. It must NOT be re-charged the
            // `tokens_before` it absorbed — that work is already done, and
            // re-charging it makes the entry permanently "heavy", so
            // successive compactions grow their selected ranges instead of
            // shrinking them (observed in the field: 246k -> 410k
            // `tokens_before` across two compactions two minutes apart,
            // re-triggering compaction immediately). Size it by the summary
            // alone, consistently with how `fit_path` renders it.
            let summary_text = summary_text_chars(summary);
            estimator.estimate(model, &summary_text).max(1)
        }
        EntryPayload::BranchSummary { summary, .. } => {
            let text = summary_text_chars(summary);
            estimator.estimate(model, &text).max(1)
        }
        EntryPayload::Custom { data, .. } => estimator.estimate(model, &data.to_string()).max(1),
        EntryPayload::CustomMessage { content, .. } => {
            let text: String = content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.as_str(),
                })
                .collect();
            estimator.estimate(model, &text).max(1)
        }
        EntryPayload::ModelChange { model } => estimator.estimate(model, model).max(1),
        EntryPayload::Label { label, .. } => {
            let text = label.as_deref().unwrap_or("");
            estimator.estimate(model, text).max(1)
        }
        EntryPayload::SessionInfo { name } => estimator.estimate(model, name).max(1),
        EntryPayload::LeafMoved { .. } => 1,
        EntryPayload::SessionEnded { reason } => estimator.estimate(model, reason).max(1),
    }
}

/// Format a `ChatMessage` into a single string for token estimation.
///
/// This is a rough approximation — we just concatenate all text content.
/// The estimator's calibration corrects for structural overhead over time.
fn format_message_text(msg: &ChatMessage) -> String {
    let mut text = String::new();
    match msg {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. } => {
            for block in content {
                let ContentBlock::Text { text: t } = block;
                text.push_str(t);
                text.push(' ');
            }
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => {
            text.push_str(tool_call_id);
            text.push(' ');
            for block in content {
                let ContentBlock::Text { text: t } = block;
                text.push_str(t);
                text.push(' ');
            }
        }
    }
    text
}

/// Format a [`CompactionSummary`] into a single string for token estimation.
///
/// Includes every field that the renderer emits (initial + current requests,
/// flat and phase-structured tool activity, flat and phase-structured key
/// findings, and notes) so the estimate reflects what the model actually sees.
pub(crate) fn summary_text_chars(summary: &CompactionSummary) -> String {
    let mut text = String::new();
    if let Some(ref req) = summary.original_request {
        text.push_str("Initial request: ");
        text.push_str(req);
        text.push('\n');
    }
    if let Some(ref req) = summary.current_request {
        text.push_str("Current request: ");
        text.push_str(req);
        text.push('\n');
    }
    // Mirror `render_compaction_summary`: phase-structured content when
    // present, otherwise the flat fallback. (Modern summaries populate
    // `phases` for any tool activity, so reading both would double-count.)
    if summary.phases.is_empty() {
        for (name, calls) in &summary.tool_calls {
            text.push_str(name);
            for call in calls {
                text.push(' ');
                text.push_str(call);
            }
            text.push('\n');
        }
        for findings in summary.key_findings.values() {
            for finding in findings {
                text.push_str(finding);
                text.push('\n');
            }
        }
    } else {
        for segment in &summary.phases {
            text.push_str(&segment.phase);
            text.push('\n');
            for (name, calls) in &segment.tool_calls {
                text.push_str(name);
                for call in calls {
                    text.push(' ');
                    text.push_str(call);
                }
                text.push('\n');
            }
            for findings in segment.key_findings.values() {
                for finding in findings {
                    text.push_str(finding);
                    text.push('\n');
                }
            }
        }
    }
    if let Some(ref notes) = summary.notes {
        text.push_str(notes);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_on_ascii() {
        assert_eq!(floor_char_boundary("hello world", 5), 5);
        assert_eq!(floor_char_boundary("hello", 10), 5); // beyond end
        assert_eq!(floor_char_boundary("hello", 0), 0);
    }

    #[test]
    fn floor_char_boundary_on_multibyte() {
        // '日' is 3 bytes. "日日日" is 9 bytes.
        let s = "日日日";
        assert_eq!(s.len(), 9);
        // index 4 is not a char boundary (日=0..3, 日=3..6, 日=6..9)
        assert_eq!(floor_char_boundary(s, 4), 3); // floor to previous boundary
        assert_eq!(floor_char_boundary(s, 6), 6); // exact boundary
        assert_eq!(floor_char_boundary(s, 8), 6); // floor to previous boundary
    }

    /// Regression test for the "compaction doesn't shrink" bug.
    ///
    /// A compaction entry that absorbed a large amount of context must be sized
    /// by its *rendered summary* for range-selection — not re-charged the
    /// `tokens_before` it absorbed. Re-charging it makes the entry permanently
    /// heavy, so successive compactions grow their selected ranges instead of
    /// shrinking them (observed: 246k -> 410k `tokens_before` across two
    /// compactions two minutes apart, re-triggering compaction immediately).
    #[test]
    fn compaction_entry_estimated_by_summary_not_tokens_before() {
        use crate::newtypes::EntryId;
        use crate::session::entry::EntryResolution;
        use crate::session::estimator::HeuristicEstimator;
        use std::collections::BTreeMap;
        use std::time::{Duration, SystemTime};

        let summary = CompactionSummary {
            original_request: Some("do something".to_owned()),
            current_request: Some("now do something else".to_owned()),
            tool_calls: BTreeMap::new(),
            key_findings: BTreeMap::new(),
            phases: Vec::new(),
            tokens_compacted: 1_000_000,
            entry_count: 999,
            time_span: Duration::from_secs(99),
            notes: None,
        };
        let entry = Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload: EntryPayload::Compaction {
                summary: summary.clone(),
                first_kept: EntryId::new(),
                tokens_before: 1_000_000,
            },
        };

        let estimator = HeuristicEstimator::new();
        let tokens = estimate_entry_tokens_for_compaction(&entry, "m", &estimator);

        // Summary text is ~60 chars (~15 tokens at chars/4). The estimate
        // must reflect that small summary, NOT the 1_000_000 tokens_before
        // (which under the old logic produced ~250_000).
        assert!(
            (1..100).contains(&tokens),
            "compaction entry must be sized by its summary ({tokens} tokens), \
             not tokens_before (1_000_000)"
        );
    }

    /// A modern mechanical summary populates BOTH the flat `tool_calls` and the
    /// phase segments with the same tool activity. The token estimate must
    /// count it once (via phases), not twice (flat + phases).
    #[test]
    fn summary_estimate_does_not_double_count_flat_and_phases() {
        use crate::newtypes::{EntryId, ToolName};
        use crate::session::entry::{CompactionPhase, EntryResolution};
        use crate::session::estimator::HeuristicEstimator;
        use std::collections::BTreeMap;
        use std::time::{Duration, SystemTime};

        fn entry(summary: CompactionSummary) -> Entry {
            Entry {
                id: EntryId::new(),
                parent_id: None,
                timestamp: SystemTime::UNIX_EPOCH,
                resolution: EntryResolution::Full,
                payload: EntryPayload::Compaction {
                    summary,
                    first_kept: EntryId::new(),
                    tokens_before: 0,
                },
            }
        }
        fn base() -> CompactionSummary {
            CompactionSummary {
                original_request: None,
                current_request: None,
                tool_calls: BTreeMap::new(),
                key_findings: BTreeMap::new(),
                phases: Vec::new(),
                tokens_compacted: 0,
                entry_count: 0,
                time_span: Duration::ZERO,
                notes: None,
            }
        }

        let mut calls = BTreeMap::new();
        calls.insert(ToolName::from("read_file"), vec!["src/main.rs".to_owned()]);
        let segment = CompactionPhase {
            phase: "exploration".to_owned(),
            tool_calls: calls.clone(),
            key_findings: BTreeMap::new(),
            user_messages: vec![],
        };

        // Activity only in phases.
        let mut phases_only = base();
        phases_only.phases = vec![segment.clone()];
        // Same activity in BOTH flat and phases (as modern summaries carry).
        let mut both = base();
        both.tool_calls = calls;
        both.phases = vec![segment];

        let est = HeuristicEstimator::new();
        let a = estimate_entry_tokens_for_compaction(&entry(phases_only), "m", &est);
        let b = estimate_entry_tokens_for_compaction(&entry(both), "m", &est);

        assert_eq!(
            a, b,
            "must not double-count tool activity present in both flat and phases"
        );
    }
}
