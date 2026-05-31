//! Session summary tool — reads the current session JSONL and returns a
//! compressed numbered history for context recovery.

use crate::error::ToolError;
use async_trait::async_trait;
use rho_core::CancellationToken;
use rho_core::tool::{Tool, ToolOutcome, ToolResult};
use rho_core::{ToolName, ToolRisk};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Maximum number of entry summaries to include (to avoid blowing context).
const MAX_SUMMARIES: usize = 50;
/// Maximum characters per entry summary line.
const MAX_LINE_CHARS: usize = 200;

/// Tool that reads the current session's JSONL file and returns a compressed
/// numbered history. This is the context-recovery safety net — when the agent
/// loses track of its task (due to context eviction), it can call this tool
/// to get a condensed view of everything that happened.
///
/// The session JSONL path is injected via an `Arc<Mutex<Option<PathBuf>>>`
/// that is set after session creation. If the path is not set or the file
/// is unreadable, the tool returns an error.
pub struct SessionSummary {
    /// Shared holder for the session JSONL path, set after session creation.
    session_path: Arc<Mutex<Option<PathBuf>>>,
}

impl SessionSummary {
    /// Create a new session summary tool.
    ///
    /// Returns the shared path holder that must be set after session creation
    /// by cloning the `Arc` and calling `set_path`.
    pub fn new() -> (Self, Arc<Mutex<Option<PathBuf>>>) {
        let path_holder = Arc::new(Mutex::new(None));
        let tool = Self {
            session_path: Arc::clone(&path_holder),
        };
        (tool, path_holder)
    }

    /// Set the session JSONL path (called after session creation).
    pub fn set_path(holder: &Arc<Mutex<Option<PathBuf>>>, path: PathBuf) {
        if let Ok(mut p) = holder.lock() {
            *p = Some(path);
        }
    }
}

#[async_trait]
impl Tool for SessionSummary {
    fn name(&self) -> ToolName {
        "session_summary".into()
    }

    fn description(&self) -> &str {
        "Read the current session's compressed history. Returns a numbered \
         list of all conversation turns (user messages, tool calls, tool results) \
         with one-line summaries. Use this when you have lost track of what \
         happened earlier in the conversation, to recover your context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> rho_core::Result<ToolOutcome> {
        let path = self
            .session_path
            .lock()
            .ok()
            .and_then(|p| p.clone())
            .ok_or_else(|| ToolError::Internal {
                message: "session path not configured".into(),
            })?;

        let content = std::fs::read_to_string(&path).map_err(|e| ToolError::Internal {
            message: format!("cannot read session: {e}"),
        })?;

        let summaries = parse_jsonl_summaries(&content, MAX_SUMMARIES, MAX_LINE_CHARS);

        let output = if summaries.is_empty() {
            "Session is empty (no entries).".to_string()
        } else {
            format!(
                "Session history ({} entries, showing last {}):\n{}",
                content.lines().count(),
                summaries.len(),
                summaries.join("\n")
            )
        };

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

/// Parse a JSONL session file into one-line summaries.
///
/// Each entry is rendered as:
/// ```text
/// N. [role] summary_text
/// ```
/// where `N` is the entry number, `role` is the message/tool role, and
/// `summary_text` is a truncated one-line description.
fn parse_jsonl_summaries(content: &str, max_entries: usize, max_line: usize) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let skip = total.saturating_sub(max_entries);

    let mut summaries = Vec::with_capacity(max_entries);
    for (i, line) in lines.iter().enumerate().skip(skip) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let summary = if let Ok(entry) = serde_json::from_str::<serde_json::Value>(trimmed) {
            format_entry_summary(&entry, max_line)
        } else {
            "(parse error)".to_string()
        };

        summaries.push(format!("{:>4}. {summary}", i + 1));
    }

    summaries
}

/// Format a single JSONL entry as a one-line summary.
fn format_entry_summary(entry: &serde_json::Value, max_chars: usize) -> String {
    let role = entry
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match role {
        "system" => "[sys] system prompt".to_string(),
        "user" => {
            let text = extract_text(entry, 150);
            truncate(&format!("[user] {text}"), max_chars)
        }
        "assistant" => {
            if entry.get("tool_calls").is_some() {
                let tools: Vec<String> = entry
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|tc| {
                                tc.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                    .map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                truncate(
                    &format!("[assistant] tool_calls: {}", tools.join(", ")),
                    max_chars,
                )
            } else {
                let text = extract_text(entry, 150);
                truncate(&format!("[assistant] {text}"), max_chars)
            }
        }
        "tool" => {
            let tool_name = entry
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let status = if entry
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|b| b.get("type"))
                .and_then(|t| t.as_str())
                == Some("error")
            {
                "ERROR"
            } else {
                "ok"
            };
            truncate(&format!("[tool:{tool_name}] {status}"), max_chars)
        }
        _ => truncate(&format!("[{role}] entry"), max_chars),
    }
}

/// Extract text content from a message entry.
fn extract_text(entry: &serde_json::Value, max_chars: usize) -> String {
    entry
        .get("content")
        .and_then(|c| {
            if c.is_string() {
                c.as_str().map(String::from)
            } else if let Some(arr) = c.as_array() {
                let texts: Vec<&str> = arr
                    .iter()
                    .filter_map(|block| {
                        if block.get("type")?.as_str()? == "text" {
                            block.get("text")?.as_str()
                        } else {
                            None
                        }
                    })
                    .collect();
                Some(texts.join(" ").chars().take(max_chars).collect())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "(no text)".to_string())
}

/// Truncate a string to at most `max_chars`, adding "…" if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let end = max_chars.saturating_sub(1);
        // Find a clean char boundary
        let mut i = end;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        format!("{}…", &s[..i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jsonl_returns_numbered_summaries() {
        let jsonl = r#"{"role":"user","content":"hello"}
{"role":"assistant","content":"hi there"}
"#;
        let summaries = parse_jsonl_summaries(jsonl, 50, 200);
        assert_eq!(summaries.len(), 2);
        assert!(summaries[0].contains("[user]"));
        assert!(summaries[0].contains("hello"));
        assert!(summaries[0].contains("1."));
        assert!(summaries[1].contains("[assistant]"));
    }

    #[test]
    fn parse_jsonl_limits_to_max_entries() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{{\"role\":\"user\",\"content\":\"msg {i}\"}}"))
            .collect();
        let jsonl = lines.join("\n");
        let summaries = parse_jsonl_summaries(&jsonl, 10, 200);
        assert_eq!(summaries.len(), 10);
        // Should show the last 10 entries (90-99)
        assert!(summaries[0].contains("msg 90"));
        assert!(summaries[9].contains("msg 99"));
    }

    #[test]
    fn parse_jsonl_handles_tool_calls() {
        let jsonl = r#"{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}}]}
{"role":"tool","tool_call_id":"c1","content":[{"type":"text","text":"file contents here"}]}
"#;
        let summaries = parse_jsonl_summaries(jsonl, 50, 200);
        assert_eq!(summaries.len(), 2);
        assert!(summaries[0].contains("tool_calls: read_file"));
        assert!(summaries[1].contains("[tool:c1]"));
        assert!(summaries[1].contains("ok"));
    }

    #[test]
    fn format_entry_summary_truncates_long_text() {
        let entry = serde_json::json!({
            "role": "user",
            "content": "a".repeat(300)
        });
        let summary = format_entry_summary(&entry, 100);
        assert!(
            summary.len() <= 102,
            "summary should be capped at max_chars + ellipsis, got {} chars: {}",
            summary.len(),
            summary
        );
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn session_summary_execute_returns_error_without_path() {
        let (tool, _holder) = SessionSummary::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            tool.execute(serde_json::Value::Null, CancellationToken::new())
                .await
        });
        assert!(result.is_err());
    }
}
