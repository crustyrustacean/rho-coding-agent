// Extension traits for typed, versioned entries in the session tree.
//
// See [`ExtensionEntry`] for out-of-band state (Attached resolution)
// and [`ExtensionMessageEntry`] for LLM-visible content (Full resolution).

use crate::message::ContentBlock;
use serde::de::DeserializeOwned;
use serde::Serialize;

// ── ExtensionEntry trait ─────────────────────────────────────────────────────

/// A trait for typed extension entries stored in the session tree.
///
/// Extensions implement this trait for their state types to get type-safe
/// read/write access to [`Custom`](super::EntryPayload::Custom) entries. The `KIND`
/// constant is stored alongside the serialized data and checked on read, so
/// schema version skew produces a clean `None` rather than a deserialization
/// error or silent corruption.
///
/// # Versioning convention
///
/// `KIND` should follow the pattern `"<author>.<feature>.v<n>"`, e.g.:
/// - `"rho.diagnostics.v1"`
/// - `"acme.linter.v2"`
///
/// When the schema changes incompatibly, bump the version number. Consumers
/// expecting the old version get `None` from [`super::Session::read_custom_state`].
///
/// # Example
///
/// ```
/// use rho_core::session::ExtensionEntry;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct DiagnosticsState {
///     error_count: usize,
/// }
///
/// impl ExtensionEntry for DiagnosticsState {
///     const KIND: &'static str = "rho.diagnostics.v1";
/// }
/// ```
pub trait ExtensionEntry: Serialize + DeserializeOwned + 'static {
    /// Namespaced kind identifier, including version.
    ///
    /// Follow the convention `"<author>.<feature>.v<n>"` to enable
    /// forward-compatible schema evolution.
    const KIND: &'static str;
}

/// A trait for typed extension *message* entries that participate in the LLM context.
///
/// This is the `CustomMessage` counterpart to [`ExtensionEntry`]. Extensions
/// implement this trait for content types that should be visible to the
/// model (resolution = Full), as opposed to [`ExtensionEntry`] which stores
/// state out-of-band (resolution = Attached).
///
/// The trait provides `content_blocks()` for writing and `from_content_blocks()`
/// for reading. The default `content_blocks()` serializes to a JSON string;
/// the default `from_content_blocks()` deserializes from the first text block.
/// Override both when you need richer formatting.
///
/// # Example
///
/// ```
/// use rho_core::session::ExtensionMessageEntry;
/// use rho_core::message::ContentBlock;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct LintSummary {
///     warnings: usize,
/// }
///
/// impl ExtensionMessageEntry for LintSummary {
///     const KIND: &'static str = "rho.lint-summary.v1";
/// }
/// ```
pub trait ExtensionMessageEntry: Serialize + DeserializeOwned + 'static {
    /// Namespaced kind identifier, including version.
    const KIND: &'static str;

    /// Serialize this entry into content blocks for storage.
    ///
    /// The default implementation serializes to a JSON string and wraps it
    /// in a single `ContentBlock::Text`. Override for richer formatting.
    fn content_blocks(&self) -> Vec<ContentBlock> {
        let json = serde_json::to_string(self).unwrap_or_default();
        vec![ContentBlock::Text { text: json }]
    }

    /// Reconstruct a typed value from content blocks.
    ///
    /// The default implementation reads the first `ContentBlock::Text` and
    /// deserializes it from JSON. Returns `None` if there are no text blocks
    /// or deserialization fails.
    fn from_content_blocks(blocks: &[ContentBlock]) -> Option<Self> {
        blocks.iter().find_map(|b| match b {
            ContentBlock::Text { text } => serde_json::from_str(text).ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    use super::*;
    use super::super::{Entry, EntryPayload, EntryResolution, Session};
    use crate::message::{ChatMessage, ContentBlock};
    use crate::newtypes::EntryId;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct DiagnosticsState {
        error_count: usize,
        last_error: Option<String>,
    }

    impl ExtensionEntry for DiagnosticsState {
        const KIND: &'static str = "rho.diagnostics.v1";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct DiagnosticsStateV2 {
        error_count: usize,
        last_error: Option<String>,
        severity: String,
    }

    impl ExtensionEntry for DiagnosticsStateV2 {
        const KIND: &'static str = "rho.diagnostics.v2";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct LintSummary {
        warnings: usize,
        files_checked: usize,
    }

    impl ExtensionMessageEntry for LintSummary {
        const KIND: &'static str = "rho.lint-summary.v1";
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct LintSummaryV2 {
        warnings: usize,
        files_checked: usize,
        errors: usize,
    }

    impl ExtensionMessageEntry for LintSummaryV2 {
        const KIND: &'static str = "rho.lint-summary.v2";
    }

    #[test]
    fn write_and_read_custom_state_round_trips() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        let state = DiagnosticsState {
            error_count: 3,
            last_error: Some("mismatched types".to_owned()),
        };
        let id = session.write_custom_state(&state);

        // The entry should have Custom payload with the correct kind
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Attached));
        if let EntryPayload::Custom { kind, data } = &entry.payload {
            assert_eq!(kind, "rho.diagnostics.v1");
            assert_eq!(data["error_count"], 3);
            assert_eq!(data["last_error"], "mismatched types");
        } else {
            panic!("expected Custom payload");
        }

        // Read back with the correct type
        let read_back: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(read_back, Some(state));
    }

    #[test]
    fn read_custom_state_returns_none_on_kind_mismatch() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        // Write as v1
        let state = DiagnosticsState {
            error_count: 1,
            last_error: None,
        };
        let id = session.write_custom_state(&state);

        // Try to read as v2 — different KIND, should get None
        let result: Option<DiagnosticsStateV2> = session.read_custom_state(&id);
        assert_eq!(result, None, "kind mismatch should return None");
    }

    #[test]
    fn read_custom_state_returns_none_on_nonexistent_entry() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result: Option<DiagnosticsState> = session.read_custom_state(&fake_id);
        assert_eq!(result, None, "nonexistent entry should return None");
    }

    #[test]
    fn read_custom_state_returns_none_on_wrong_payload_type() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        // Write a CustomMessage (Full resolution), not a Custom (Attached)
        let id = session.append_custom_message(
            "rho.diagnostics.v1".to_owned(),
            vec![ContentBlock::Text {
                text: "hello".to_owned(),
            }],
        );

        // Try to read as Custom state — wrong payload type, should get None
        let result: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(result, None, "wrong payload type should return None");
    }

    #[test]
    fn write_custom_state_multiple_entries_independent() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        let state1 = DiagnosticsState {
            error_count: 1,
            last_error: Some("e1".to_owned()),
        };
        let state2 = DiagnosticsState {
            error_count: 2,
            last_error: Some("e2".to_owned()),
        };

        let id1 = session.write_custom_state(&state1);
        let id2 = session.write_custom_state(&state2);

        let read1: Option<DiagnosticsState> = session.read_custom_state(&id1);
        let read2: Option<DiagnosticsState> = session.read_custom_state(&id2);

        assert_eq!(read1, Some(state1));
        assert_eq!(read2, Some(state2));
    }

    #[test]
    fn write_and_read_custom_message_round_trips() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let id = session.write_custom_message(&summary);

        // The entry should have CustomMessage payload with the correct kind
        let entry = session.entry(&id).unwrap();
        assert!(matches!(entry.resolution, EntryResolution::Full));
        if let EntryPayload::CustomMessage { kind, content } = &entry.payload {
            assert_eq!(kind, "rho.lint-summary.v1");
            assert!(!content.is_empty());
        } else {
            panic!("expected CustomMessage payload");
        }

        // Read back with the correct type
        let read_back: Option<LintSummary> = session.read_custom_message(&id);
        assert_eq!(read_back, Some(summary));
    }

    #[test]
    fn read_custom_message_returns_none_on_kind_mismatch() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        // Write as v1
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let id = session.write_custom_message(&summary);

        // Try to read as v2 — different KIND, should get None
        let result: Option<LintSummaryV2> = session.read_custom_message(&id);
        assert_eq!(result, None, "kind mismatch should return None");
    }

    #[test]
    fn read_custom_message_returns_none_on_nonexistent_entry() {
        let session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let fake_id = EntryId::new();
        let result: Option<LintSummary> = session.read_custom_message(&fake_id);
        assert_eq!(result, None, "nonexistent entry should return None");
    }

    #[test]
    fn read_custom_message_returns_none_on_wrong_payload_type() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        // Write a Custom (Attached), not a CustomMessage (Full)
        let id = session.append_custom_state(
            "rho.lint-summary.v1".to_owned(),
            serde_json::json!({"warnings": 5}),
        );

        // Try to read as CustomMessage — wrong payload type, should get None
        let result: Option<LintSummary> = session.read_custom_message(&id);
        assert_eq!(result, None, "wrong payload type should return None");
    }

    #[test]
    fn extension_entry_kind_versioning_produces_clean_break() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");

        // Write v1
        let v1 = DiagnosticsState {
            error_count: 3,
            last_error: Some("type error".to_owned()),
        };
        let id = session.write_custom_state(&v1);

        // v1 reads back fine
        let v1_read: Option<DiagnosticsState> = session.read_custom_state(&id);
        assert_eq!(v1_read, Some(v1.clone()));

        // v2 reads None (clean break)
        let v2_read: Option<DiagnosticsStateV2> = session.read_custom_state(&id);
        assert_eq!(v2_read, None);

        // Now write v2 and verify v2 reads fine
        let v2 = DiagnosticsStateV2 {
            error_count: 3,
            last_error: Some("type error".to_owned()),
            severity: "high".to_owned(),
        };
        let id2 = session.write_custom_state(&v2);
        let v2_read2: Option<DiagnosticsStateV2> = session.read_custom_state(&id2);
        assert_eq!(v2_read2, Some(v2));

        // v1 cannot read the v2 entry
        let v1_read2: Option<DiagnosticsState> = session.read_custom_state(&id2);
        assert_eq!(v1_read2, None);
    }

    #[test]
    fn custom_state_is_filtered_from_path_messages() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Write a custom state entry (Attached resolution)
        let state = DiagnosticsState {
            error_count: 3,
            last_error: None,
        };
        session.write_custom_state(&state);

        let messages = session.path_messages();

        // Only System + User should appear; the Custom state is Attached
        assert_eq!(messages.len(), 2, "only System and User should appear");
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
    }

    #[test]
    fn custom_message_appears_in_path_messages() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Write a custom message entry (Full resolution)
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        session.write_custom_message(&summary);

        let messages = session.path_messages();

        // System + User + CustomMessage (rendered as User)
        assert!(messages.len() >= 3, "custom message should appear in path");

        // The custom message should be rendered as a User message (by fit_path)
        let has_custom = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("warnings")
                })
            } else {
                false
            }
        });
        assert!(
            has_custom,
            "custom message content should be visible to the model"
        );
    }

    #[test]
    fn extension_message_entry_default_content_blocks_round_trips() {
        let summary = LintSummary {
            warnings: 5,
            files_checked: 12,
        };
        let blocks = summary.content_blocks();
        assert!(!blocks.is_empty());

        // Default implementation serializes to JSON
        let ContentBlock::Text { text } = &blocks[0];
        assert!(text.contains("warnings"), "should contain field names");

        // from_content_blocks should reconstruct the value
        let reconstructed: Option<LintSummary> = LintSummary::from_content_blocks(&blocks);
        assert_eq!(reconstructed, Some(summary));
    }

    #[test]
    fn extension_message_entry_from_empty_blocks_returns_none() {
        let result: Option<LintSummary> = LintSummary::from_content_blocks(&[]);
        assert_eq!(result, None, "empty blocks should return None");
    }
}
