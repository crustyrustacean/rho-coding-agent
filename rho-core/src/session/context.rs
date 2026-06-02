// Context building — path_messages, send_current, compact_older_than, context_stats.

use super::{ContextStats, Entry, EntryId, EntryResolution, Session};
use crate::error::Result;
use crate::message::ChatMessage;
use crate::session::error::SessionError;

impl Session {
    // ── Compaction ────────────────────────────────────────────────────────

    /// Compact the oldest entries whose total estimated tokens exceed
    /// `threshold`, using the given [`CompactionStrategy`](super::compaction::CompactionStrategy).
    ///
    /// This is the core compaction operation. It:
    /// 1. Walks the leaf-to-root path and selects the oldest contiguous
    ///    entries whose cumulative estimated tokens exceed `threshold`.
    /// 2. Calls `strategy.compact()` on the selected entries to produce a
    ///    [`CompactionSummary`].
    /// 3. Appends a [`Compaction`](super::EntryPayload::Compaction) entry to the
    ///    tree.
    /// 4. Transitions the compacted entries' resolution from `Full` to
    ///    `Compacted { into }`, where `into` is the new Compaction entry's
    ///    ID.
    ///
    /// The compacted entries are **not deleted** — they remain in the tree
    /// at lower resolution, accessible via [`entry()`](Session::entry), but
    /// bypassed by [`path_messages()`](Session::path_messages) and
    /// [`fit_path`](crate::context::ContextManager::fit_path).
    ///
    /// # Invariants
    ///
    /// - The system message (root entry) is never compacted.
    /// - The most recent entry (the leaf) is never compacted.
    /// - At least one entry remains un-compacted after this operation.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`](crate::error::RhoError) if the compaction strategy fails.
    ///
    /// # Returns
    ///
    /// The [`EntryId`] of the new Compaction entry.
    pub async fn compact_older_than(
        &mut self,
        threshold: usize,
        strategy: &dyn super::compaction::CompactionStrategy,
    ) -> Result<EntryId> {
        // Walk the leaf-to-root path and reverse to get chronological order.
        let path = self.path_to_root();
        let chronological: Vec<&Entry> = path.into_iter().rev().collect();

        if chronological.len() <= 1 {
            // Nothing to compact (only the root, or empty)
            return Err(SessionError::Persistence(
                "cannot compact: session has too few entries".to_string(),
            )
            .into());
        }

        // Find the contiguous range of oldest entries whose tokens exceed
        // the threshold. We never compact the root (index 0) or the leaf
        // (last entry).
        let mut cumulative_tokens: usize = 0;
        let mut compact_end: usize = 0; // exclusive upper bound; 0 means not reached

        // Start from index 1 (skip the root system message)
        for (i, entry) in chronological.iter().enumerate().skip(1) {
            // Don't compact the last entry (the leaf)
            if i == chronological.len() - 1 {
                break;
            }

            // Only compact Full-resolution entries
            if !matches!(entry.resolution, EntryResolution::Full) {
                continue;
            }

            cumulative_tokens += super::truncation::estimate_entry_tokens_for_compaction(
                entry,
                &self.model,
                self.estimator.as_ref(),
            );
            compact_end = i + 1;

            if cumulative_tokens >= threshold {
                break;
            }
        }

        // If we didn't accumulate enough tokens, there's nothing to compact.
        if cumulative_tokens < threshold || compact_end <= 1 {
            return Err(SessionError::Persistence(
                "cannot compact: not enough full-resolution entries exceeding threshold"
                    .to_string(),
            )
            .into());
        }

        // Collect the entries to compact (indices 1..compact_end)
        let to_compact: Vec<&Entry> = chronological[1..compact_end].to_vec();
        if to_compact.is_empty() {
            return Err(SessionError::Persistence(
                "cannot compact: no entries selected".to_string(),
            )
            .into());
        }

        // Collect the IDs of entries to compact BEFORE any mutation
        let compacted_ids: Vec<EntryId> = to_compact.iter().map(|e| e.id.clone()).collect();
        let first_kept_id = chronological[compact_end].id.clone();
        let tokens_before = cumulative_tokens;

        // Generate the summary
        let summary = strategy.compact(&to_compact).await?;

        // Append the Compaction entry
        let compaction_id = self.append_compaction(summary, first_kept_id, tokens_before);

        // Transition the compacted entries' resolution to Compacted
        for entry_id in &compacted_ids {
            if let Some(entry) = self.entries.get_mut(entry_id) {
                entry.resolution = EntryResolution::Compacted {
                    into: compaction_id.clone(),
                };
            }
        }

        Ok(compaction_id)
    }

    // ── Context building ────────────────────────────────────────────────

    /// Build the message list for the model from the leaf-to-root path.
    ///
    /// Returns the messages that would be sent to the model, after
    /// applying resolution filtering and overhead subtraction via
    /// [`ContextManager::fit_path`](crate::context::ContextManager::fit_path).
    ///
    /// Returns the fitted messages in chronological order (system first),
    /// ready to be placed in a [`ChatRequest`](crate::request::ChatRequest).
    pub fn path_messages(&self) -> Vec<ChatMessage> {
        // Walk leaf-to-root, then reverse to get chronological order.
        let path = self.path_to_root();
        let entries: Vec<&Entry> = path.into_iter().rev().collect();

        self.context_manager.fit_path(
            &entries,
            self.token_budget,
            self.estimator.as_ref(),
            &self.tools,
        )
    }

    /// Send the current session state to the model and persist the response.
    ///
    /// 1. Builds the message list via [`path_messages`](Self::path_messages).
    /// 2. Constructs an [`LlmRequest`](rho_ai::LlmRequest).
    /// 3. Calls the LLM service.
    /// 4. Persists the assistant response as an appended entry.
    /// 5. Calibrates the estimator against the actual `prompt_tokens`.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`](crate::error::RhoError) if the HTTP request fails
    /// or the response cannot be parsed.
    pub async fn send_current(
        &mut self,
        client: &dyn rho_ai::LlmService,
    ) -> crate::error::Result<crate::conversation::AssistantResponse> {
        use crate::agent::{consume_stream, route_response};

        let fitted = self.path_messages();

        // Estimate tokens for the request before sending (for calibration).
        let estimated_tokens = Self::estimate_messages_tokens(&fitted);

        let llm_messages: Vec<rho_ai::LlmMessage> =
            fitted.iter().map(ChatMessage::to_llm_message).collect();

        let llm_request = rho_ai::LlmRequest {
            model: self.model.clone(),
            messages: llm_messages,
            tools: self.tools.clone(),
            max_tokens: Some(self.token_budget.completion_reserve),
        };

        let event_stream = client.chat_stream(llm_request).await.map_err(|e| {
            crate::error::RhoError::Client(crate::client::error::ClientError::from(e))
        })?;

        let events = consume_stream(event_stream, &crate::agent::NopObserver).await?;
        let acc = rho_ai::StreamEvent::accumulate(&events);

        // Calibrate estimator if the API returned prompt_tokens.
        if acc.usage.input_tokens > 0 {
            self.estimator.calibrate(
                &self.model,
                estimated_tokens,
                usize::try_from(acc.usage.input_tokens).unwrap_or(0),
            );
        }

        route_response(&acc, self)
    }

    /// Estimate the total tokens for a slice of messages.
    fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
        use crate::context::approximate_tokens;
        // We use approximate_tokens for a consistent per-message estimate,
        // then scale by the estimator's model-specific ratio.
        // For now, approximate_tokens gives chars/4 which is close enough
        // for calibration. The estimator's calibrate() will correct the
        // ratio anyway.
        messages.iter().map(approximate_tokens).sum()
    }

    /// Compute estimated context window usage statistics.
    ///
    /// Returns a [`ContextStats`] snapshot describing how full the context
    /// window is, how many entries are in the active conversation path,
    /// and how much budget remains.
    ///
    /// Uses the session's calibrated estimator for the best available
    /// approximation. The fitted messages are the ones that would actually
    /// be sent to the model (after eviction).
    pub fn context_stats(&self) -> ContextStats {
        let budget = self.token_budget;
        let messages = self.path_messages();
        let used = Self::estimate_messages_tokens(&messages);
        ContextStats {
            context_window: budget.context_window,
            completion_reserve: budget.completion_reserve,
            estimated_used: used,
            message_count: messages.len(),
            entry_count: self.entries.len(),
            path_entry_count: self.path_to_root().len(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    use super::super::{CompactionSummary, EntryPayload};
    use super::*;
    use crate::context::TokenBudget;
    use crate::message::{ChatMessage, ContentBlock};
    use crate::newtypes::ToolName;

    // ── Context building tests (Task 8) ────────────────────────────────

    #[test]
    fn path_messages_returns_chronological_messages() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));
        session.append_user_message("how are you?");

        let messages = session.path_messages();

        // Should be in chronological order: System, User, Assistant, User
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
        assert!(matches!(messages[2], ChatMessage::Assistant { .. }));
        assert!(matches!(messages[3], ChatMessage::User { .. }));
    }

    #[test]
    fn path_messages_filters_compacted_entries() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("hi"));

        // Manually mark the user entry as compacted
        if let Some(entry) = session.entries.get_mut(&user_id) {
            entry.resolution = EntryResolution::Compacted {
                into: EntryId::from("test"),
            };
        }

        let messages = session.path_messages();

        // The compacted user message should NOT appear
        let has_hello = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("hello")
                })
            } else {
                false
            }
        });
        assert!(!has_hello, "compacted entry should be filtered out");

        // The assistant message should still appear
        let has_hi = messages.iter().any(|m| {
            if let ChatMessage::Assistant {
                content,
                tool_calls,
            } = m
            {
                tool_calls.is_empty()
                    && content.iter().any(|b| {
                        let ContentBlock::Text { text } = b;
                        text.contains("hi")
                    })
            } else {
                false
            }
        });
        assert!(has_hi, "non-compacted entry should be present");
    }

    #[test]
    fn path_messages_filters_attached_entries() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");

        // Add an Attached entry (label)
        let leaf_id = session.leaf().unwrap();
        session.append_label(leaf_id, Some("checkpoint".to_owned()));

        let messages = session.path_messages();

        // Only System + User should appear; the Label (Attached) should be filtered
        assert_eq!(messages.len(), 2, "only System and User should appear");
        assert!(matches!(messages[0], ChatMessage::System { .. }));
        assert!(matches!(messages[1], ChatMessage::User { .. }));
    }

    #[test]
    fn path_messages_renders_compaction_summary() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("fix the bug");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));

        let summary = CompactionSummary {
            original_request: Some("fix the bug".to_owned()),
            tool_calls: {
                let mut map = std::collections::BTreeMap::new();
                map.insert(ToolName::from("read_file"), vec!["src/main.rs".to_owned()]);
                map
            },
            tokens_compacted: 1024,
            entry_count: 3,
            time_span: std::time::Duration::from_secs(45),
            notes: Some("compacted for budget".to_owned()),
        };

        let first_kept = session.leaf().unwrap();
        session.append_compaction(summary, first_kept, 2048);

        let messages = session.path_messages();

        // The compaction summary should be rendered as a synthetic User message
        let compaction_msg = messages.iter().find(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            compaction_msg.is_some(),
            "compaction should render as User message"
        );

        // The message should contain the original request and tool activity
        if let ChatMessage::User { content } = compaction_msg.unwrap() {
            let ContentBlock::Text { text } = &content[0];
            assert!(
                text.contains("fix the bug"),
                "should contain original request"
            );
            assert!(text.contains("read_file"), "should contain tool name");
            assert!(text.contains("1 calls"), "should contain call count");
            assert!(text.contains("src/main.rs"), "should contain args summary");
            assert!(
                text.contains("compacted for budget"),
                "should contain notes"
            );
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn path_messages_renders_branch_summary() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        let from_id = session.append_user_message("hello");

        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 50,
            entry_count: 1,
            time_span: std::time::Duration::from_secs(10),
            notes: None,
        };

        session
            .branch_with_summary(&root_id, summary, from_id)
            .unwrap();

        let messages = session.path_messages();

        // The branch summary should be rendered as a synthetic User message
        let summary_msg = messages.iter().find(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            summary_msg.is_some(),
            "branch summary should render as User message"
        );
    }

    #[test]
    fn path_messages_subtracts_tool_schema_overhead() {
        use serde_json::json;

        // Create a session with a very small budget and tool schemas
        let tools = vec![
            rho_ai::ToolDefinition::new(
                "read_file",
                "Read a file",
                json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            rho_ai::ToolDefinition::new(
                "run_command",
                "Execute a command",
                json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            ),
        ];

        let mut session = Session::in_memory("m", Some("sys"), tools, "/tmp")
            .with_token_budget(TokenBudget::with_reserve(500, 50));

        // Add enough messages that some would need to be evicted
        for i in 0..20 {
            session.append_user_message(&format!(
                "message {i} with some padding text to make it longer"
            ));
            session.append_assistant_message(ChatMessage::assistant_text(format!("reply {i}")));
        }

        let messages = session.path_messages();

        // The messages should fit within the adjusted budget (which is smaller
        // than the raw budget because tool schema overhead was subtracted)
        // At minimum, the system message should survive
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, ChatMessage::System { .. })),
            "system message should always survive"
        );
    }

    #[test]
    fn path_messages_without_tools_has_no_schema_overhead() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp")
            .with_token_budget(TokenBudget::new(32_768));

        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let messages = session.path_messages();
        // With no tools and a generous budget, all messages should fit
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn path_messages_after_branch_excludes_old_branch() {
        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        session.append_user_message("hello");
        let _asst_a_id = session.append_assistant_message(ChatMessage::assistant_text("reply A"));

        // Branch back to root
        session.branch_to(&root_id).unwrap();
        session.append_user_message("different question");

        let messages = session.path_messages();

        // Should contain: System, LeafMoved (Attached → filtered), User("different question")
        // Should NOT contain "reply A" or "hello"
        let has_reply_a = messages.iter().any(|m| {
            if let ChatMessage::Assistant {
                content,
                tool_calls,
            } = m
            {
                tool_calls.is_empty()
                    && content.iter().any(|b| {
                        let ContentBlock::Text { text } = b;
                        text.contains("reply A")
                    })
            } else {
                false
            }
        });
        assert!(!has_reply_a, "old branch should not appear");

        let has_different = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("different question")
                })
            } else {
                false
            }
        });
        assert!(has_different, "new question should appear");
    }

    #[test]
    fn path_messages_empty_session() {
        let session = Session::in_memory("m", None, vec![], "/tmp");
        let messages = session.path_messages();
        assert!(messages.is_empty());
    }

    #[test]
    fn compaction_summary_rendering_is_deterministic() {
        use crate::context::render_compaction_summary;

        let mut tool_calls = std::collections::BTreeMap::new();
        tool_calls.insert(
            ToolName::from("read_file"),
            vec!["a.rs".to_owned(), "b.rs".to_owned()],
        );

        let summary = CompactionSummary {
            original_request: Some("fix it".to_owned()),
            tool_calls,
            tokens_compacted: 500,
            entry_count: 7,
            time_span: std::time::Duration::from_secs(30),
            notes: Some("notes here".to_owned()),
        };

        let msg1 = render_compaction_summary(&summary);
        let msg2 = render_compaction_summary(&summary);

        // BTreeMap guarantees iteration order, so rendering is deterministic
        assert_eq!(
            msg1, msg2,
            "rendering should be byte-stable for fixed input"
        );

        // Verify the text content contains the expected parts
        if let ChatMessage::User { content } = &msg1 {
            let ContentBlock::Text { text } = &content[0];
            assert!(text.contains("[Compacted: 7 entries, 500 tokens"));
            assert!(text.contains("Original request: \"fix it\""));
            assert!(text.contains("read_file: 2 calls"));
            assert!(text.contains("a.rs, b.rs"));
            assert!(text.contains("notes here"));
        } else {
            panic!("expected User message");
        }
    }

    #[test]
    fn compaction_summary_rendering_without_optional_fields() {
        use crate::context::render_compaction_summary;

        let summary = CompactionSummary {
            original_request: None,
            tool_calls: std::collections::BTreeMap::new(),
            tokens_compacted: 100,
            entry_count: 2,
            time_span: std::time::Duration::from_secs(5),
            notes: None,
        };

        let msg = render_compaction_summary(&summary);
        if let ChatMessage::User { content } = &msg {
            let ContentBlock::Text { text } = &content[0];
            assert!(text.contains("[Compacted: 2 entries, 100 tokens"));
            assert!(!text.contains("Original request"));
            assert!(!text.contains("Tool activity"));
        } else {
            panic!("expected User message");
        }
    }

    // ── compact_older_than tests (Task 9) ───────────────────────────────

    #[tokio::test]
    async fn compact_older_than_transitions_resolution() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        let user_id = session.append_user_message("fix the bug");
        let _asst_id = session.append_assistant_message(ChatMessage::assistant_text("ok"));
        let _user2_id = session.append_user_message("read another file");

        // Compact with a very low threshold (1 token) to force compaction
        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        // The compacted entries should now have Compacted resolution
        let user_entry = session.entry(&user_id).unwrap();
        assert!(
            matches!(
                &user_entry.resolution,
                EntryResolution::Compacted { into } if *into == compaction_id
            ),
            "compacted entry should have Compacted resolution pointing to the compaction entry"
        );

        // The compaction entry itself should be Full resolution
        let compaction_entry = session.entry(&compaction_id).unwrap();
        assert!(matches!(compaction_entry.resolution, EntryResolution::Full));
    }

    #[tokio::test]
    async fn compact_older_than_preserves_original_request() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("find the secret: TIGER-7742");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));
        session.append_user_message("now read another file");

        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        // The compaction summary should contain the original request
        let compaction_entry = session.entry(&compaction_id).unwrap();
        if let EntryPayload::Compaction { summary, .. } = &compaction_entry.payload {
            assert_eq!(
                summary.original_request,
                Some("find the secret: TIGER-7742".to_owned())
            );
        } else {
            panic!("expected Compaction payload");
        }
    }

    #[tokio::test]
    async fn compact_older_than_compacted_entries_filtered_from_path() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("fix the bug");
        session.append_assistant_message(ChatMessage::assistant_text("ok"));
        session.append_user_message("read another file");

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        let messages = session.path_messages();

        // The compacted user message should NOT appear as a raw User message
        // in path_messages (it may appear inside the compaction summary text)
        let has_raw_user_msg = messages.iter().any(|m| {
            // We look for a User message that is NOT the compaction summary
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    // A raw user message would just be "fix the bug", not the
                    // formatted compaction summary
                    text.trim() == "fix the bug"
                })
            } else {
                false
            }
        });
        assert!(
            !has_raw_user_msg,
            "compacted entry should not appear as a raw User message in path_messages"
        );

        // The compaction summary SHOULD appear as a synthetic User message
        let has_compacted = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("[Compacted:")
                })
            } else {
                false
            }
        });
        assert!(
            has_compacted,
            "compaction summary should appear in path_messages"
        );

        // The non-compacted entries should still be present
        let has_another = messages.iter().any(|m| {
            if let ChatMessage::User { content } = m {
                content.iter().any(|b| {
                    let ContentBlock::Text { text } = b;
                    text.contains("read another file")
                })
            } else {
                false
            }
        });
        assert!(
            has_another,
            "non-compacted entry should appear in path_messages"
        );

        // Verify the entry is indeed Compacted
        let compacted_entry = session.entry(&user_id).unwrap();
        assert!(
            matches!(
                compacted_entry.resolution,
                EntryResolution::Compacted { .. }
            ),
            "compacted entry should have Compacted resolution"
        );
    }

    #[tokio::test]
    async fn compact_older_than_does_not_compact_root() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("system prompt"), vec![], "/tmp");
        let root_id = session.leaf().unwrap();
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The root (system message) should never be compacted
        let root_entry = session.entry(&root_id).unwrap();
        assert!(
            matches!(root_entry.resolution, EntryResolution::Full),
            "root (system message) should never be compacted"
        );
    }

    #[tokio::test]
    async fn compact_older_than_does_not_compact_leaf() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let leaf_before = session.leaf().unwrap();

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The leaf should have moved (to the compaction entry),
        // so the original leaf should still be Full resolution
        let leaf_entry = session.entry(&leaf_before).unwrap();
        assert!(
            matches!(leaf_entry.resolution, EntryResolution::Full),
            "last entry before compaction should not be compacted"
        );
    }

    #[tokio::test]
    async fn compact_older_than_errors_on_too_few_entries() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        // Only the root entry exists — can't compact
        let strategy = MechanicalCompactionStrategy::new();
        let result = session.compact_older_than(1, &strategy).await;
        assert!(result.is_err(), "should error with too few entries");
    }

    #[tokio::test]
    async fn compact_older_than_errors_when_threshold_not_exceeded() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        // Set a huge threshold that won't be reached
        let strategy = MechanicalCompactionStrategy::new();
        let result = session.compact_older_than(1_000_000, &strategy).await;
        assert!(result.is_err(), "should error when threshold not exceeded");
    }

    #[tokio::test]
    async fn compact_older_than_compacted_entries_still_in_tree() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let user_id = session.append_user_message("hello");
        session.append_assistant_message(ChatMessage::assistant_text("hi"));

        let strategy = MechanicalCompactionStrategy::new();
        session.compact_older_than(1, &strategy).await.unwrap();

        // The compacted entry should still be accessible via entry()
        let entry = session.entry(&user_id);
        assert!(
            entry.is_some(),
            "compacted entry should still be in the tree"
        );
    }

    #[tokio::test]
    async fn compact_older_than_first_kept_references_valid_entry() {
        use crate::session::compaction::MechanicalCompactionStrategy;

        let mut session = Session::in_memory("m", Some("sys"), vec![], "/tmp");
        let _root_id = session.leaf().unwrap();
        session.append_user_message("msg1");
        let asst_id = session.append_assistant_message(ChatMessage::assistant_text("reply1"));
        let _last_user_id = session.append_user_message("msg2");

        let strategy = MechanicalCompactionStrategy::new();
        let compaction_id = session.compact_older_than(1, &strategy).await.unwrap();

        let compaction_entry = session.entry(&compaction_id).unwrap();
        if let EntryPayload::Compaction { first_kept, .. } = &compaction_entry.payload {
            assert!(
                session.entry(first_kept).is_some(),
                "first_kept should reference a valid entry"
            );
            // With threshold=1, only the first entry after root gets compacted
            // (it exceeds threshold immediately), so first_kept is the assistant entry
            assert_eq!(
                *first_kept, asst_id,
                "first_kept should be the first entry after the compacted range"
            );
        } else {
            panic!("expected Compaction payload");
        }
    }
}
