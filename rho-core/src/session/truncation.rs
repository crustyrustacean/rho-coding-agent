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
    estimator: &dyn TokenEstimator,
) -> usize {
    // Use the estimator's ratio: if a string of length L has T tokens,
    // then chars/token ≈ L/T, so chars ≈ target_tokens * (L/T).
    // But we can also just iterate: start from the full string and shrink.
    // For simplicity, use a proportional estimate.
    let total_tokens = estimator.estimate(s);
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
    estimator: &dyn TokenEstimator,
) -> usize {
    match &entry.payload {
        EntryPayload::Message(msg) => {
            let text = format_message_text(msg);
            estimator.estimate(&text).max(1)
        }
        EntryPayload::Compaction {
            summary,
            tokens_before,
            ..
        } => {
            // For existing compaction entries, use the recorded tokens_before
            // plus the summary's own token cost
            let summary_text = format_compaction_summary_text(summary);
            *tokens_before + estimator.estimate(&summary_text)
        }
        EntryPayload::BranchSummary { summary, .. } => {
            let text = format_compaction_summary_text(summary);
            estimator.estimate(&text).max(1)
        }
        EntryPayload::Custom { data, .. } => estimator.estimate(&data.to_string()).max(1),
        EntryPayload::CustomMessage { content, .. } => {
            let text: String = content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => text.as_str(),
                })
                .collect();
            estimator.estimate(&text).max(1)
        }
        EntryPayload::ModelChange { model } => estimator.estimate(model).max(1),
        EntryPayload::Label { label, .. } => {
            let text = label.as_deref().unwrap_or("");
            estimator.estimate(text).max(1)
        }
        EntryPayload::SessionInfo { name } => estimator.estimate(name).max(1),
        EntryPayload::LeafMoved { .. } => 1,
        EntryPayload::SessionEnded { reason } => estimator.estimate(reason).max(1),
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

/// Format a `CompactionSummary` into a single string for token estimation.
fn format_compaction_summary_text(summary: &CompactionSummary) -> String {
    let mut text = String::new();
    if let Some(ref req) = summary.original_request {
        text.push_str(req);
        text.push(' ');
    }
    for (name, calls) in &summary.tool_calls {
        text.push_str(name);
        for call in calls {
            text.push(' ');
            text.push_str(call);
        }
    }
    if let Some(ref notes) = summary.notes {
        text.push(' ');
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
}
