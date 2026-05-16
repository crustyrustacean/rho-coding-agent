//! Streaming types for chat completions.
//!
//! [`StreamChunk`] is the unified streaming event type. Real SSE connections
//! produce many small deltas; the default [`ChatClient::chat_stream`] wrapper
//! converts a full [`ModelResponse`] into a `Vec<StreamChunk>` via
//! [`StreamChunk::from_response`].

use crate::response::{FinishReason, ModelResponse};

// ── StreamChunk ───────────────────────────────────────────────────────────────

/// A single event from a streaming chat completion response.
///
/// Produced by [`ChatClient::chat_stream`] — either incrementally from an SSE
/// connection or, for the default fallback, as a batch converted from a full
/// [`ModelResponse`].
///
/// [ChatClient::chat_stream]: crate::client::ChatClient::chat_stream
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamChunk {
    /// Incremental text content.
    TextDelta(String),
    /// Incremental reasoning / chain-of-thought content.
    ReasoningDelta(String),
    /// Incremental tool call information.
    ToolCallDelta {
        /// Index of the tool call (for matching start/delta pairs).
        index: usize,
        /// The tool call ID (present on the first delta for this index).
        id: Option<String>,
        /// The function name (present on the first delta for this index).
        function_name: Option<String>,
        /// Incremental arguments JSON.
        arguments_delta: Option<String>,
    },
    /// The stream has completed.
    Done(FinishReason),
}

impl StreamChunk {
    /// Convert a full [`ModelResponse`] into a sequence of [`StreamChunk`]s.
    ///
    /// This is used by the default [`ChatClient::chat_stream`] implementation
    /// to wrap a non-streaming response into the streaming API.
    ///
    /// Produces:
    /// - `TextDelta` if the response has non-empty text content
    /// - `ReasoningDelta` if the response has non-empty reasoning content
    /// - `ToolCallDelta` for each tool call
    /// - `Done` with the finish reason
    ///
    /// [ChatClient::chat_stream]: crate::client::ChatClient::chat_stream
    pub fn from_response(response: &ModelResponse) -> Vec<Self> {
        let mut chunks = Vec::new();

        if let Some(choice) = response.choices.first() {
            if !choice.message.content.is_empty() {
                chunks.push(Self::TextDelta(choice.message.content.clone()));
            }
            if !choice.message.reasoning_content.is_empty() {
                chunks.push(Self::ReasoningDelta(
                    choice.message.reasoning_content.clone(),
                ));
            }
            for (index, tc) in choice.message.tool_calls.iter().enumerate() {
                chunks.push(Self::ToolCallDelta {
                    index,
                    id: Some(tc.id.to_string()),
                    function_name: Some(tc.function.name.to_string()),
                    arguments_delta: Some(tc.function.arguments.clone()),
                });
            }
            chunks.push(Self::Done(choice.finish_reason.clone()));
        }

        chunks
    }

    /// Accumulate a stream of chunks into an [`AccumulatedResponse`].
    ///
    /// Collects text, reasoning, and tool call deltas into complete strings
    /// and returns the finish reason.
    pub fn accumulate(chunks: &[StreamChunk]) -> AccumulatedResponse {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<AccumulatedToolCall> = Vec::new();
        let mut finish_reason = FinishReason::Stop;

        for chunk in chunks {
            match chunk {
                Self::TextDelta(t) => text.push_str(t),
                Self::ReasoningDelta(r) => reasoning.push_str(r),
                Self::ToolCallDelta {
                    index,
                    id,
                    function_name,
                    arguments_delta,
                } => {
                    // Ensure we have a slot for this index.
                    if tool_calls.len() <= *index {
                        tool_calls.resize_with(*index + 1, AccumulatedToolCall::default);
                    }
                    let tc = &mut tool_calls[*index];
                    if let Some(id) = id {
                        tc.id = Some(id.clone());
                    }
                    if let Some(name) = function_name {
                        tc.function_name = Some(name.clone());
                    }
                    if let Some(delta) = arguments_delta {
                        tc.arguments.push_str(delta);
                    }
                }
                Self::Done(reason) => {
                    finish_reason = reason.clone();
                }
            }
        }

        AccumulatedResponse {
            text,
            reasoning,
            tool_calls,
            finish_reason,
        }
    }
}

// ── Accumulated types ─────────────────────────────────────────────────────────

/// The result of accumulating a stream of [`StreamChunk`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccumulatedResponse {
    /// Accumulated text content.
    pub text: String,
    /// Accumulated reasoning content.
    pub reasoning: String,
    /// Accumulated tool calls.
    pub tool_calls: Vec<AccumulatedToolCall>,
    /// The finish reason from the `Done` chunk.
    pub finish_reason: FinishReason,
}

/// A tool call accumulated from streaming deltas.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccumulatedToolCall {
    /// The tool call ID.
    pub id: Option<String>,
    /// The function name.
    pub function_name: Option<String>,
    /// The accumulated arguments JSON.
    pub arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ModelToolCall, ToolCallFunction};

    fn text_model_response(content: &str) -> ModelResponse {
        ModelResponse {
            id: "test-id".to_owned(),
            object: "chat.completion".to_owned(),
            created: 0,
            model: "test-model".to_owned(),
            choices: vec![crate::response::ModelChoice {
                index: 0,
                message: crate::response::ModelMessage {
                    content: content.to_owned(),
                    reasoning_content: String::new(),
                    tool_calls: vec![],
                },
                logprobs: None,
                finish_reason: FinishReason::Stop,
            }],
            usage: crate::response::ModelUsage::default(),
            stats: crate::response::ModelStats {},
            system_fingerprint: String::new(),
        }
    }

    fn reasoning_model_response(content: &str, reasoning: &str) -> ModelResponse {
        ModelResponse {
            id: "test-id".to_owned(),
            object: "chat.completion".to_owned(),
            created: 0,
            model: "test-model".to_owned(),
            choices: vec![crate::response::ModelChoice {
                index: 0,
                message: crate::response::ModelMessage {
                    content: content.to_owned(),
                    reasoning_content: reasoning.to_owned(),
                    tool_calls: vec![],
                },
                logprobs: None,
                finish_reason: FinishReason::Stop,
            }],
            usage: crate::response::ModelUsage::default(),
            stats: crate::response::ModelStats {},
            system_fingerprint: String::new(),
        }
    }

    fn tool_call_model_response(calls: Vec<(&str, &str, &str)>) -> ModelResponse {
        let tool_calls: Vec<ModelToolCall> = calls
            .into_iter()
            .map(|(id, name, args)| ModelToolCall {
                id: crate::newtypes::ToolCallId::new(id.to_owned()),
                call_type: "function".to_owned(),
                function: ToolCallFunction {
                    name: crate::newtypes::ToolName::new(name.to_owned()),
                    arguments: args.to_owned(),
                },
            })
            .collect();
        ModelResponse {
            id: "test-id".to_owned(),
            object: "chat.completion".to_owned(),
            created: 0,
            model: "test-model".to_owned(),
            choices: vec![crate::response::ModelChoice {
                index: 0,
                message: crate::response::ModelMessage {
                    content: String::new(),
                    reasoning_content: String::new(),
                    tool_calls,
                },
                logprobs: None,
                finish_reason: FinishReason::ToolCalls,
            }],
            usage: crate::response::ModelUsage::default(),
            stats: crate::response::ModelStats {},
            system_fingerprint: String::new(),
        }
    }

    fn length_model_response(content: &str) -> ModelResponse {
        ModelResponse {
            id: "test-id".to_owned(),
            object: "chat.completion".to_owned(),
            created: 0,
            model: "test-model".to_owned(),
            choices: vec![crate::response::ModelChoice {
                index: 0,
                message: crate::response::ModelMessage {
                    content: content.to_owned(),
                    reasoning_content: String::new(),
                    tool_calls: vec![],
                },
                logprobs: None,
                finish_reason: FinishReason::Length,
            }],
            usage: crate::response::ModelUsage::default(),
            stats: crate::response::ModelStats {},
            system_fingerprint: String::new(),
        }
    }

    // ── from_response ────────────────────────────────────────────────────

    #[test]
    fn from_response_text_produces_text_delta_and_done() {
        let response = text_model_response("hello world");
        let chunks = StreamChunk::from_response(&response);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], StreamChunk::TextDelta("hello world".to_owned()));
        assert_eq!(chunks[1], StreamChunk::Done(FinishReason::Stop));
    }

    #[test]
    fn from_response_empty_text_produces_only_done() {
        let response = text_model_response("");
        let chunks = StreamChunk::from_response(&response);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], StreamChunk::Done(FinishReason::Stop));
    }

    #[test]
    fn from_response_with_reasoning_produces_reasoning_delta() {
        let response = reasoning_model_response("answer", "let me think");
        let chunks = StreamChunk::from_response(&response);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], StreamChunk::TextDelta("answer".to_owned()));
        assert_eq!(
            chunks[1],
            StreamChunk::ReasoningDelta("let me think".to_owned())
        );
        assert_eq!(chunks[2], StreamChunk::Done(FinishReason::Stop));
    }

    #[test]
    fn from_response_tool_calls_produces_tool_call_deltas() {
        let response = tool_call_model_response(vec![
            ("call-1", "read_file", r#"{"path":"foo.rs"}"#),
            ("call-2", "edit_file", r#"{"path":"bar.rs"}"#),
        ]);
        let chunks = StreamChunk::from_response(&response);
        assert_eq!(chunks.len(), 3); // 2 tool call deltas + Done
        assert_eq!(
            chunks[0],
            StreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".to_owned()),
                function_name: Some("read_file".to_owned()),
                arguments_delta: Some(r#"{"path":"foo.rs"}"#.to_owned()),
            }
        );
        assert_eq!(
            chunks[1],
            StreamChunk::ToolCallDelta {
                index: 1,
                id: Some("call-2".to_owned()),
                function_name: Some("edit_file".to_owned()),
                arguments_delta: Some(r#"{"path":"bar.rs"}"#.to_owned()),
            }
        );
        assert_eq!(chunks[2], StreamChunk::Done(FinishReason::ToolCalls));
    }

    #[test]
    fn from_response_length_finish_reason_preserved() {
        let response = length_model_response("partial");
        let chunks = StreamChunk::from_response(&response);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1], StreamChunk::Done(FinishReason::Length));
    }

    #[test]
    fn from_response_with_no_choices_produces_empty_vec() {
        let response = ModelResponse {
            id: "test-id".to_owned(),
            object: "chat.completion".to_owned(),
            created: 0,
            model: "test-model".to_owned(),
            choices: vec![],
            usage: crate::response::ModelUsage::default(),
            stats: crate::response::ModelStats {},
            system_fingerprint: String::new(),
        };
        let chunks = StreamChunk::from_response(&response);
        assert!(chunks.is_empty());
    }

    // ── accumulate ───────────────────────────────────────────────────────

    #[test]
    fn accumulate_text_deltas() {
        let chunks = vec![
            StreamChunk::TextDelta("hello ".to_owned()),
            StreamChunk::TextDelta("world".to_owned()),
            StreamChunk::Done(FinishReason::Stop),
        ];
        let acc = StreamChunk::accumulate(&chunks);
        assert_eq!(acc.text, "hello world");
        assert_eq!(acc.finish_reason, FinishReason::Stop);
        assert!(acc.tool_calls.is_empty());
    }

    #[test]
    fn accumulate_tool_call_deltas() {
        let chunks = vec![
            StreamChunk::ToolCallDelta {
                index: 0,
                id: Some("call-1".to_owned()),
                function_name: Some("read_file".to_owned()),
                arguments_delta: Some(r#"{"path":"#.to_owned()),
            },
            StreamChunk::ToolCallDelta {
                index: 0,
                id: None,
                function_name: None,
                arguments_delta: Some(r#""foo.rs"}"#.to_owned()),
            },
            StreamChunk::Done(FinishReason::ToolCalls),
        ];
        let acc = StreamChunk::accumulate(&chunks);
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, Some("call-1".to_owned()));
        assert_eq!(
            acc.tool_calls[0].function_name,
            Some("read_file".to_owned())
        );
        assert_eq!(acc.tool_calls[0].arguments, r#"{"path":"foo.rs"}"#);
        assert_eq!(acc.finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn accumulate_multiple_tool_calls() {
        let chunks = vec![
            StreamChunk::ToolCallDelta {
                index: 0,
                id: Some("c1".to_owned()),
                function_name: Some("read_file".to_owned()),
                arguments_delta: Some(r#"{"path":"a.rs"}"#.to_owned()),
            },
            StreamChunk::ToolCallDelta {
                index: 1,
                id: Some("c2".to_owned()),
                function_name: Some("edit_file".to_owned()),
                arguments_delta: Some(r#"{"path":"b.rs"}"#.to_owned()),
            },
            StreamChunk::Done(FinishReason::ToolCalls),
        ];
        let acc = StreamChunk::accumulate(&chunks);
        assert_eq!(acc.tool_calls.len(), 2);
        assert_eq!(
            acc.tool_calls[0].function_name,
            Some("read_file".to_owned())
        );
        assert_eq!(
            acc.tool_calls[1].function_name,
            Some("edit_file".to_owned())
        );
    }

    #[test]
    fn accumulate_reasoning_deltas() {
        let chunks = vec![
            StreamChunk::ReasoningDelta("step 1".to_owned()),
            StreamChunk::ReasoningDelta(" step 2".to_owned()),
            StreamChunk::TextDelta("answer".to_owned()),
            StreamChunk::Done(FinishReason::Stop),
        ];
        let acc = StreamChunk::accumulate(&chunks);
        assert_eq!(acc.reasoning, "step 1 step 2");
        assert_eq!(acc.text, "answer");
    }

    #[test]
    fn accumulate_empty_chunks_gives_defaults() {
        let chunks: Vec<StreamChunk> = vec![];
        let acc = StreamChunk::accumulate(&chunks);
        assert!(acc.text.is_empty());
        assert!(acc.reasoning.is_empty());
        assert!(acc.tool_calls.is_empty());
        assert_eq!(acc.finish_reason, FinishReason::Stop);
    }
}
