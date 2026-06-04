//! Entry outlining and summarization — reduced-fidelity text generation.
//!
//! Phase 1 uses generic truncation as the outline/summary text.
//! Phase 2 replaces these with tool-specific structural summaries.

use crate::message::{ChatMessage, ContentBlock};
use crate::session::entry::{Entry, EntryPayload};

/// Generate a generic outline for an entry.
///
/// Phase 1: truncates to first 200 chars.
/// Phase 2: tool-specific structural summaries.
pub(crate) fn generate_outline(entry: &Entry) -> String {
    let text = extract_text_content(entry);
    truncate_with_ellipsis(&text, 200)
}

/// Generate a generic summary for an entry.
///
/// Phase 1: truncates to first 80 chars.
/// Phase 2: prose summaries or key-value pairs.
pub(crate) fn generate_summary(entry: &Entry) -> String {
    let text = extract_text_content(entry);
    truncate_with_ellipsis(&text, 80)
}

/// Truncate text to `max_len` characters at a UTF-8-safe boundary,
/// appending "…" if truncated.
fn truncate_with_ellipsis(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_owned();
    }
    // Find a char boundary at or before max_len.
    let mut end = max_len;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Extract text content from an entry for outline/summary generation.
fn extract_text_content(entry: &Entry) -> String {
    match &entry.payload {
        EntryPayload::Message(msg) => extract_message_text(msg),
        EntryPayload::CustomMessage { content, .. } => content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Extract text from a `ChatMessage`.
fn extract_message_text(msg: &ChatMessage) -> String {
    match msg {
        ChatMessage::Tool { content, .. }
        | ChatMessage::User { content }
        | ChatMessage::Assistant { content, .. }
        | ChatMessage::System { content } => content
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ChatMessage;
    use crate::newtypes::EntryId;
    use crate::session::entry::{Entry, EntryPayload, EntryResolution};
    use std::time::SystemTime;

    fn test_entry(payload: EntryPayload) -> Entry {
        Entry {
            id: EntryId::new(),
            parent_id: None,
            timestamp: SystemTime::UNIX_EPOCH,
            resolution: EntryResolution::Full,
            payload,
        }
    }

    #[test]
    fn generate_outline_truncates_long_text() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text(
            "x".repeat(500),
        )));
        let outline = generate_outline(&entry);
        // Should be ~200 chars + "…"
        assert!(
            outline.len() <= 204,
            "outline should be ~200 chars, got {}",
            outline.len()
        );
        assert!(outline.ends_with('…'), "outline should end with ellipsis");
    }

    #[test]
    fn generate_outline_preserves_short_text() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text("hello")));
        let outline = generate_outline(&entry);
        assert_eq!(outline, "hello");
    }

    #[test]
    fn generate_summary_truncates_long_text() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text(
            "y".repeat(300),
        )));
        let summary = generate_summary(&entry);
        assert!(
            summary.len() <= 84,
            "summary should be ~80 chars, got {}",
            summary.len()
        );
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn generate_summary_preserves_short_text() {
        let entry = test_entry(EntryPayload::Message(ChatMessage::user_text("ok")));
        let summary = generate_summary(&entry);
        assert_eq!(summary, "ok");
    }

    #[test]
    fn generate_outline_handles_empty_entry() {
        let entry = test_entry(EntryPayload::Label {
            target_id: EntryId::new(),
            label: None,
        });
        let outline = generate_outline(&entry);
        assert_eq!(outline, "");
    }

    #[test]
    fn truncate_with_ellipsis_utf8_safe() {
        // "α" is 2 bytes in UTF-8. Truncating at byte 1 should back up to 0.
        let text = "αβγδεζηθ"; // 8 chars, 16 bytes
        let result = truncate_with_ellipsis(text, 5); // 5 bytes falls in the middle of 'γ'
        assert!(result.ends_with('…'));
        // Should not panic and should be valid UTF-8
        let _ = result.len();
    }

    #[test]
    fn truncate_with_ellipsis_no_truncation_needed() {
        let text = "short";
        let result = truncate_with_ellipsis(text, 100);
        assert_eq!(result, "short");
        assert!(!result.ends_with('…'));
    }
}
