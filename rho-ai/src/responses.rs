//! Native `OpenAI` Responses API provider.
//!
//! This module converts rho's provider-neutral request types into the
//! `/v1/responses` wire format. Streaming response handling and HTTP routing
//! are layered on top of these adapters.

#![expect(
    dead_code,
    reason = "request adapters are wired into the HTTP service in Phase 2"
)]

use crate::error::ProviderError;
use crate::types::{LlmMessage, LlmRequest, ToolDefinition};
use serde::Serialize;

/// Request body sent to the `OpenAI` Responses endpoint.
#[derive(Debug, Serialize)]
struct ResponsesRequest {
    /// Model identifier.
    model: String,
    /// Stateless conversation input.
    input: Vec<InputItem>,
    /// System/developer instructions extracted from the conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    /// Function tools available to the model.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<FunctionTool>,
    /// Maximum number of output tokens, including reasoning tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<usize>,
    /// Reasoning configuration for reasoning-capable models.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    /// Enable SSE streaming.
    stream: bool,
    /// Disable provider-side response storage; rho owns conversation state.
    store: bool,
    /// Allow the model to request multiple function calls in one response.
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
}

/// A stateless Responses API input item.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputItem {
    /// User- or assistant-authored text.
    Message {
        /// Message role.
        role: MessageRole,
        /// Plain-text message content.
        content: String,
    },
    /// A function call made by the assistant in an earlier turn.
    FunctionCall {
        /// Stable call identifier used to correlate the function output.
        call_id: String,
        /// Function name.
        name: String,
        /// JSON-encoded function arguments.
        arguments: String,
    },
    /// The result of a function call.
    FunctionCallOutput {
        /// Identifier of the function call that produced this output.
        call_id: String,
        /// Plain-text function output.
        output: String,
    },
}

/// Role for a message input item.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    /// User-authored input.
    User,
    /// Model-authored output retained in stateless history.
    Assistant,
}

/// A custom function tool in the Responses wire format.
#[derive(Debug, Serialize)]
struct FunctionTool {
    /// Always `"function"`.
    r#type: &'static str,
    /// Function name.
    name: String,
    /// Human-readable function description.
    description: String,
    /// JSON Schema for function arguments.
    parameters: serde_json::Value,
    /// Whether `OpenAI` strict-schema validation is enabled.
    strict: bool,
}

/// Reasoning options accepted by reasoning-capable Responses models.
#[derive(Debug, Serialize)]
struct ReasoningConfig {
    /// Requested reasoning effort.
    effort: String,
    /// Request a streamed summary rather than hidden chain of thought.
    summary: &'static str,
}

/// Build the URL for the Responses endpoint.
///
/// Accepts an API base URL or a full Chat Completions/Responses URL. Known
/// endpoint suffixes are removed before `/responses` is appended, making the
/// operation idempotent and compatible with existing rho configurations.
fn responses_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let base = trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| trimmed.strip_suffix("/responses"))
        .unwrap_or(trimmed);
    format!("{base}/responses")
}

/// Convert unified tools into the flat Responses function-tool shape.
fn build_tools(tools: &[ToolDefinition]) -> Vec<FunctionTool> {
    tools
        .iter()
        .map(|tool| FunctionTool {
            r#type: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
            // rho's schemas predate OpenAI strict mode and may use constructs
            // that strict mode rejects. Keep existing permissive behavior.
            strict: false,
        })
        .collect()
}

/// Split system instructions from heterogeneous Responses input items.
fn build_input(messages: &[LlmMessage]) -> (Option<String>, Vec<InputItem>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();

    for message in messages {
        match message {
            LlmMessage::System(content) => instructions.push(content.clone()),
            LlmMessage::User(content) => input.push(InputItem::Message {
                role: MessageRole::User,
                content: content.clone(),
            }),
            LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    input.push(InputItem::Message {
                        role: MessageRole::Assistant,
                        content: content.clone(),
                    });
                }
                input.extend(tool_calls.iter().map(|call| InputItem::FunctionCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }));
            }
            LlmMessage::Tool {
                tool_call_id,
                content,
            } => input.push(InputItem::FunctionCallOutput {
                call_id: tool_call_id.clone(),
                output: content.clone(),
            }),
        }
    }

    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join("\n\n"))
    };
    (instructions, input)
}

/// Convert a provider-neutral request into a Responses request body.
fn build_request(request: &LlmRequest) -> Result<ResponsesRequest, ProviderError> {
    if request.model.is_empty() {
        return Err(ProviderError::Response {
            message: "model identifier is empty".to_owned(),
            raw: None,
        });
    }

    let (instructions, input) = build_input(&request.messages);
    let tools = build_tools(&request.tools);
    let reasoning = request
        .reasoning_effort
        .as_ref()
        .map(|effort| ReasoningConfig {
            effort: effort.clone(),
            summary: "auto",
        });

    Ok(ResponsesRequest {
        model: request.model.clone(),
        input,
        instructions,
        parallel_tool_calls: (!tools.is_empty()).then_some(true),
        tools,
        max_output_tokens: request.max_tokens,
        reasoning,
        stream: true,
        store: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCall;

    /// Serialize a unified request through the Responses adapter.
    fn request_json(request: &LlmRequest) -> serde_json::Value {
        serde_json::to_value(build_request(request).expect("valid request"))
            .expect("request serializes")
    }

    #[test]
    fn responses_url_appends_to_base() {
        assert_eq!(
            responses_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn responses_url_normalizes_known_suffixes_and_slashes() {
        for endpoint in [
            "https://api.openai.com/v1/",
            "https://api.openai.com/v1/responses",
            "https://api.openai.com/v1/responses/",
            "https://api.openai.com/v1/chat/completions",
            "https://api.openai.com/v1/chat/completions/",
        ] {
            assert_eq!(
                responses_url(endpoint),
                "https://api.openai.com/v1/responses",
                "failed for {endpoint}"
            );
        }
    }

    #[test]
    fn system_messages_become_joined_instructions() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![
                LlmMessage::System("first".into()),
                LlmMessage::User("hello".into()),
                LlmMessage::System("second".into()),
            ],
        );
        let json = request_json(&request);
        assert_eq!(json["instructions"], "first\n\nsecond");
        assert_eq!(json["input"].as_array().unwrap().len(), 1);
        assert_eq!(json["input"][0]["role"], "user");
    }

    #[test]
    fn instructions_are_omitted_when_absent() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("instructions").is_none());
    }

    #[test]
    fn mixed_assistant_text_and_calls_preserve_order() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![LlmMessage::Assistant {
                content: Some("checking".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "call_a".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"a.rs"}"#.into(),
                    },
                    ToolCall {
                        id: "call_b".into(),
                        name: "list_dir".into(),
                        arguments: r#"{"path":"src"}"#.into(),
                    },
                ],
            }],
        );
        let json = request_json(&request);
        assert_eq!(json["input"][0]["type"], "message");
        assert_eq!(json["input"][0]["role"], "assistant");
        assert_eq!(json["input"][1]["type"], "function_call");
        assert_eq!(json["input"][1]["call_id"], "call_a");
        assert_eq!(json["input"][1]["name"], "read_file");
        assert_eq!(json["input"][2]["call_id"], "call_b");
    }

    #[test]
    fn tool_result_becomes_function_call_output() {
        let request = LlmRequest::new(
            "gpt-5",
            vec![LlmMessage::Tool {
                tool_call_id: "call_exact".into(),
                content: "the result".into(),
            }],
        );
        let json = request_json(&request);
        assert_eq!(json["input"][0]["type"], "function_call_output");
        assert_eq!(json["input"][0]["call_id"], "call_exact");
        assert_eq!(json["input"][0]["output"], "the result");
    }

    #[test]
    fn function_tools_use_flat_permissive_shape() {
        let request =
            LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]).with_tools(vec![
                ToolDefinition::new(
                    "read_file",
                    "Read a file",
                    serde_json::json!({"type": "object"}),
                ),
            ]);
        let json = request_json(&request);
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["name"], "read_file");
        assert_eq!(json["tools"][0]["description"], "Read a file");
        assert_eq!(json["tools"][0]["parameters"]["type"], "object");
        assert_eq!(json["tools"][0]["strict"], false);
        assert!(json["tools"][0].get("function").is_none());
        assert_eq!(json["parallel_tool_calls"], true);
    }

    #[test]
    fn empty_tools_omit_tools_and_parallel_flag() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("tools").is_none());
        assert!(json.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn limits_reasoning_and_stateless_streaming_serialize() {
        let mut request =
            LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]).with_max_tokens(4096);
        request.reasoning_effort = Some("high".into());
        let json = request_json(&request);
        assert_eq!(json["max_output_tokens"], 4096);
        assert_eq!(json["reasoning"]["effort"], "high");
        assert_eq!(json["reasoning"]["summary"], "auto");
        assert_eq!(json["stream"], true);
        assert_eq!(json["store"], false);
        assert!(json.get("max_tokens").is_none());
        assert!(json.get("reasoning_effort").is_none());
    }

    #[test]
    fn reasoning_is_omitted_when_not_configured() {
        let request = LlmRequest::new("gpt-5", vec![LlmMessage::User("hello".into())]);
        let json = request_json(&request);
        assert!(json.get("reasoning").is_none());
        assert!(json.get("max_output_tokens").is_none());
    }

    #[test]
    fn empty_model_is_rejected() {
        let request = LlmRequest::new("", vec![LlmMessage::User("hello".into())]);
        let error = build_request(&request).expect_err("empty model must fail");
        assert!(error.to_string().contains("model identifier is empty"));
    }
}
