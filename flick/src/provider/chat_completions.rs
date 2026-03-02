use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::Client;
use smallvec::SmallVec;

use crate::config::CompatFlags;
use crate::context::{ContentBlock, Message, Role};
use crate::error::ProviderError;
use crate::event::StreamEvent;
use crate::model::openai_reasoning_effort;
use crate::provider::sse::{self, EventBatch, SseAction};
use crate::provider::{EventStream, Provider, RequestParams, ToolDefinition};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

pub struct ChatCompletionsProvider {
    base_url: String,
    api_key: String,
    compat: CompatFlags,
    client: Client,
}

impl ChatCompletionsProvider {
    #[allow(clippy::expect_used)] // Client::new() panics on same failure
    pub fn new(base_url: String, api_key: String, compat: CompatFlags) -> Self {
        Self {
            base_url,
            api_key,
            compat,
            client: Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    /// Base URL for test verification.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Compat flags for test verification.
    pub const fn compat(&self) -> &CompatFlags {
        &self.compat
    }

    fn build_body(&self, params: &RequestParams<'_>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": params.model,
            "stream": true,
        });

        if !self.compat.skip_stream_options {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

        // Reasoning models require max_completion_tokens instead of max_tokens
        if params.reasoning.is_some() {
            body["max_completion_tokens"] = serde_json::json!(params.max_tokens);
        } else {
            body["max_tokens"] = serde_json::json!(params.max_tokens);
        }

        // OpenAI reasoning models reject requests containing temperature.
        if params.reasoning.is_none() {
            if let Some(temp) = params.temperature {
                body["temperature"] = serde_json::json!(temp);
            }
        }

        let mut messages: Vec<serde_json::Value> = Vec::new();
        if let Some(system) = params.system_prompt {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        for m in params.messages {
            messages.extend(convert_message(m));
        }
        body["messages"] = serde_json::Value::Array(messages);

        if !params.tools.is_empty() {
            let tools: Vec<serde_json::Value> = params
                .tools
                .iter()
                .map(convert_tool)
                .collect();
            body["tools"] = serde_json::Value::Array(tools);

            if self.compat.explicit_tool_choice_auto {
                body["tool_choice"] = serde_json::json!("auto");
            }
        }

        if let Some(level) = params.reasoning {
            body["reasoning_effort"] = serde_json::json!(openai_reasoning_effort(level));
        }

        if let Some(schema) = params.output_schema {
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "output",
                    "schema": schema,
                    "strict": true,
                }
            });
        }

        body
    }
}

/// Reject requests that specify both tools and `output_schema`, which are mutually
/// exclusive in the `OpenAI` API.
fn validate_params(params: &RequestParams<'_>) -> Result<(), ProviderError> {
    if !params.tools.is_empty() && params.output_schema.is_some() {
        return Err(ProviderError::SseParse(
            "tools and output_schema cannot be used together".into(),
        ));
    }
    Ok(())
}

impl Provider for ChatCompletionsProvider {
    async fn stream(
        &self,
        params: RequestParams<'_>,
    ) -> Result<EventStream, ProviderError> {
        validate_params(&params)?;
        let body = self.build_body(&params);
        let url = format!("{}/v1/chat/completions", self.base_url);

        sse::stream_request(
            || {
                self.client
                    .post(&url)
                    .header("authorization", format!("Bearer {}", self.api_key))
                    .header("content-type", "application/json")
                    .json(&body)
            },
            parse_openai_sse,
        )
        .await
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        validate_params(&params)?;
        Ok(self.build_body(&params))
    }
}

/// Returns one API message per tool result, flattened by the caller.
fn convert_message(msg: &Message) -> Vec<serde_json::Value> {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };

    let mut has_tool_results = false;
    let mut has_tool_use = false;
    for block in &msg.content {
        match block {
            ContentBlock::ToolResult { .. } => has_tool_results = true,
            ContentBlock::ToolUse { .. } => has_tool_use = true,
            _ => {}
        }
    }

    if has_tool_results {
        let mut messages = Vec::new();
        // Preserve any Text blocks as a preceding user message
        let text: String = msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if !text.is_empty() {
            messages.push(serde_json::json!({"role": "user", "content": text}));
        }
        messages.extend(msg.content.iter().filter_map(|block| {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                let output = if *is_error {
                    format!("Error: {content}")
                } else {
                    content.clone()
                };
                Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": output,
                }))
            } else {
                None
            }
        }));
        return messages;
    }

    if role == "assistant" && has_tool_use {
        let mut text_content = String::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => text_content.push_str(text),
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": input.to_string(),
                        }
                    }));
                }
                _ => {}
            }
        }
        let mut msg_json = serde_json::json!({"role": "assistant"});
        msg_json["content"] = if text_content.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(text_content)
        };
        if !tool_calls.is_empty() {
            msg_json["tool_calls"] = serde_json::Value::Array(tool_calls);
        }
        return vec![msg_json];
    }

    let text: String = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    vec![serde_json::json!({"role": role, "content": text})]
}

fn convert_tool(tool: &ToolDefinition) -> serde_json::Value {
    let mut params = tool
        .input_schema
        .clone()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
    if params.get("type").is_none() {
        params["type"] = serde_json::json!("object");
    }
    // OpenAI requires "properties" on object schemas
    if params.get("properties").is_none() {
        params["properties"] = serde_json::json!({});
    }
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": params,
        }
    })
}

/// Per-tool-call accumulated state.
struct ToolCallState {
    id: String,
    accumulated_args: String,
}

#[allow(clippy::too_many_lines)]
fn parse_openai_sse(
    byte_stream: impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
        + Send
        + 'static,
) -> impl tokio_stream::Stream<Item = Result<StreamEvent, ProviderError>> + Send {
    // BTreeMap for deterministic iteration order
    let mut current_tool_calls: BTreeMap<u64, ToolCallState> = BTreeMap::new();
    sse::spawn_sse_parser(byte_stream, sse::DEFAULT_SSE_IDLE_TIMEOUT, move |block: &str| {
        let mut all_events: EventBatch = SmallVec::new();

        for line in block.lines() {
            let Some(data) = line.strip_prefix("data:").map(|rest| rest.strip_prefix(' ').unwrap_or(rest)) else {
                continue;
            };

            if data == "[DONE]" {
                // Drain pending tool calls before ending stream
                for state in current_tool_calls.values() {
                    all_events.push(StreamEvent::ToolCallEnd {
                        call_id: state.id.clone(),
                        arguments: state.accumulated_args.clone(),
                    });
                }
                current_tool_calls.clear();
                if all_events.is_empty() {
                    return Ok(SseAction::Done);
                }
                return Ok(SseAction::DoneWithEvents(all_events));
            }

            let parsed: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| ProviderError::SseParse(format!("OpenAI SSE: {e}")))?;

            // Handle mid-stream error objects from OpenAI
            if let Some(error) = parsed.get("error").and_then(|e| e.as_object()) {
                let message = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                let code = error
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("error");
                all_events.push(StreamEvent::Error {
                    message: message.to_string(),
                    code: code.to_string(),
                    fatal: true,
                });
                return Ok(SseAction::DoneWithEvents(all_events));
            }

            let usage = &parsed["usage"];
            if !usage.is_null() {
                if let Some((input_tokens, output_tokens)) =
                    crate::provider::extract_token_pair(usage, "prompt_tokens", "completion_tokens")
                {
                    if input_tokens > 0 || output_tokens > 0 {
                        let cached = usage
                            .get("prompt_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        all_events.push(StreamEvent::Usage {
                            input_tokens,
                            output_tokens,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: cached,
                        });
                    }
                }
            }

            let Some(choices) = parsed["choices"].as_array() else {
                continue;
            };

            for choice in choices {
                let delta = &choice["delta"];

                // Text content
                if let Some(text) = delta["content"].as_str() {
                    if !text.is_empty() {
                        all_events.push(StreamEvent::TextDelta {
                            text: text.to_string(),
                        });
                    }
                }

                // Tool calls
                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                    for tc in tool_calls {
                        let index = tc["index"].as_u64().unwrap_or(0);
                        let function = &tc["function"];

                        if let Some(id) = tc["id"].as_str() {
                            // Only insert if this index is not already tracked
                            if let std::collections::btree_map::Entry::Vacant(entry) = current_tool_calls.entry(index) {
                                // Reject empty/missing tool name
                                let name = function["name"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .ok_or_else(|| ProviderError::SseParse(
                                        "tool call missing name".into(),
                                    ))?
                                    .to_string();
                                entry.insert(ToolCallState {
                                    id: id.to_string(),
                                    accumulated_args: String::new(),
                                });
                                all_events.push(StreamEvent::ToolCallStart {
                                    call_id: id.to_string(),
                                    tool_name: name,
                                });
                            }
                        } else if !current_tool_calls.contains_key(&index) {
                            // First chunk for this index has no id — tool call cannot be tracked
                            return Err(ProviderError::SseParse(
                                format!("tool call at index {index} missing id"),
                            ));
                        }

                        if let Some(args_delta) = function["arguments"].as_str() {
                            if !args_delta.is_empty() {
                                if let Some(state) = current_tool_calls.get_mut(&index) {
                                    state.accumulated_args.push_str(args_delta);
                                    all_events.push(StreamEvent::ToolCallDelta {
                                        call_id: state.id.clone(),
                                        arguments_delta: args_delta.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }

                match choice["finish_reason"].as_str() {
                    // Only emit ToolCallEnd when finish_reason is "tool_calls"
                    Some("tool_calls") => {
                        for state in current_tool_calls.values() {
                            all_events.push(StreamEvent::ToolCallEnd {
                                call_id: state.id.clone(),
                                arguments: state.accumulated_args.clone(),
                            });
                        }
                        current_tool_calls.clear();
                    }
                    // Surface truncation so the caller knows the response was cut short.
                    // Non-fatal: the response received so far is valid.
                    Some("length") => {
                        all_events.push(StreamEvent::Error {
                            message: "model response truncated (max tokens exceeded)".into(),
                            code: "max_tokens".into(),
                            fatal: false,
                        });
                        current_tool_calls.clear();
                    }
                    // Surface content policy violations. Non-fatal: partial
                    // content may still be usable.
                    Some("content_filter") => {
                        all_events.push(StreamEvent::Error {
                            message: "response blocked by content filter".into(),
                            code: "content_filter".into(),
                            fatal: false,
                        });
                        current_tool_calls.clear();
                    }
                    // Model decided not to call tools — discard pending
                    // tool calls so [DONE] drain doesn't emit ToolCallEnd
                    // with empty arguments.
                    Some("stop") => {
                        current_tool_calls.clear();
                    }
                    _ => {}
                }
            }
        }

        Ok(SseAction::Events(all_events))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ContentBlock, Message, Role};

    #[test]
    fn convert_message_multiple_tool_results() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "result 1".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_2".into(),
                    content: "result 2".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_3".into(),
                    content: "result 3".into(),
                    is_error: true,
                },
            ],
        };

        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 3, "should emit one message per tool result");

        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_1");
        assert_eq!(messages[0]["content"], "result 1");

        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_2");
        assert_eq!(messages[1]["content"], "result 2");

        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_3");
        assert_eq!(messages[2]["content"], "Error: result 3");
    }

    #[test]
    fn convert_message_plain_text() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hello");
    }

    #[test]
    fn convert_message_assistant_with_tool_use() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "calling tool".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp/test"}),
                },
            ],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "calling tool");
        assert!(messages[0]["tool_calls"].is_array());
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
    }

    use super::super::test_helpers::{byte_stream, minimal_params};

    fn make_provider() -> ChatCompletionsProvider {
        ChatCompletionsProvider::new(
            "https://api.example.com".into(),
            "test-key".into(),
            CompatFlags::default(),
        )
    }

    #[test]
    fn build_body_minimal() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(body.get("temperature").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_with_system_prompt() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: Some("Be helpful"),
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        let empty = vec![];
        let messages = body["messages"].as_array().unwrap_or(&empty);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be helpful");
    }

    #[test]
    fn build_body_with_tools_and_explicit_choice() {
        let provider = ChatCompletionsProvider::new(
            "https://api.example.com".into(),
            "test-key".into(),
            CompatFlags {
                explicit_tool_choice_auto: true,
                ..CompatFlags::default()
            },
        );
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: Some(serde_json::json!({"type": "object"})),
        }];
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert!(body["tools"].is_array());
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn build_body_with_reasoning() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "o3-mini",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: Some(crate::model::ReasoningLevel::High),
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["max_completion_tokens"], 1024);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_body_with_output_schema() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let schema = serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}});
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: Some(&schema),
        };
        let body = provider.build_body(&params);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "output");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }

    // -- parse_openai_sse byte-stream tests -----------------------------------

    async fn collect_openai_events(chunks: Vec<&str>) -> Vec<StreamEvent> {
        use tokio_stream::StreamExt;
        let stream = parse_openai_sse(byte_stream(chunks));
        tokio::pin!(stream);
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        events
    }

    #[tokio::test]
    async fn parse_openai_sse_text_deltas_and_done() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello", " world"]);
    }

    #[tokio::test]
    async fn parse_openai_sse_tool_call_sequence() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\": \\\"/tmp\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert!(matches!(&events[0], StreamEvent::ToolCallStart { call_id, tool_name }
            if call_id == "call_1" && tool_name == "read_file"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { call_id, .. }
            if call_id == "call_1"));
        assert!(matches!(&events[2], StreamEvent::ToolCallDelta { call_id, .. }
            if call_id == "call_1"));
        assert!(matches!(&events[3], StreamEvent::ToolCallEnd { call_id, arguments }
            if call_id == "call_1" && arguments == "{\"path\": \"/tmp\"}"));
    }

    #[tokio::test]
    async fn parse_openai_sse_usage_tokens() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        let usage = events.iter().find(|e| matches!(e, StreamEvent::Usage { .. }));
        assert!(matches!(usage, Some(StreamEvent::Usage { input_tokens: 10, output_tokens: 5, .. })));
    }

    #[test]
    fn convert_message_tool_call_only_has_null_content() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            }],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert!(messages[0]["content"].is_null());
        assert!(messages[0]["tool_calls"].is_array());
    }

    #[test]
    fn convert_message_tool_result_with_is_error() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_err".into(),
                content: "file not found".into(),
                is_error: true,
            }],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Error: file not found");
    }

    #[tokio::test]
    async fn parse_openai_sse_finish_reason_stop_drains_on_done() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"test\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        // finish_reason "stop" clears pending tool calls — no ToolCallEnd emitted
        assert_eq!(
            events.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).count(),
            0,
            "finish_reason stop should discard pending tool calls"
        );
    }

    #[tokio::test]
    async fn parse_openai_sse_parallel_tool_calls() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"write_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"p\\\":\\\"a\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"p\\\":\\\"b\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::ToolCallStart { .. })).count(), 2);
        assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::ToolCallEnd { .. })).count(), 2);
    }

    #[tokio::test]
    async fn parse_openai_sse_usage_only_no_choices() {
        let events = collect_openai_events(vec![
            "data: {\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert!(matches!(&events[0], StreamEvent::Usage { input_tokens: 42, output_tokens: 7, .. }));
    }

    #[test]
    fn convert_message_thinking_only() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "reasoning".into(),
                signature: "sig_123".into(),
            }],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        // Thinking blocks are not tool-use, so falls through to text extraction
        // which yields empty text since no Text blocks exist
        assert_eq!(messages[0]["role"], "assistant");
    }

    #[tokio::test]
    async fn parse_openai_sse_malformed_json_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_openai_sse(byte_stream(vec![
            "data: {not valid}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some());
        assert!(first.as_ref().is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn parse_openai_sse_chunks_split_mid_block() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{",
            "\"content\":\"split\"}}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["split"]);
    }

    #[tokio::test]
    async fn parse_openai_sse_done_mid_stream_stops() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"before\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"after\"}}]}\n\n",
        ]).await;

        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["before"]);
    }

    // -- wiremock-based Provider::stream tests --

    #[tokio::test]
    async fn stream_text_response_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path, header};
        use tokio_stream::StreamExt;

        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from mock\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}],\"usage\":{\"prompt_tokens\":15,\"completion_tokens\":4}}\n\n",
            "data: [DONE]\n\n",
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = ChatCompletionsProvider::new(
            server.uri(),
            "test-key".into(),
            CompatFlags::default(),
        );
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 100,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };

        let Ok(stream) = provider.stream(params).await else {
            panic!("stream should succeed");
        };
        tokio::pin!(stream);
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            let Ok(event) = item else { panic!("event should be Ok") };
            events.push(event);
        }

        let texts: Vec<&str> = events.iter().filter_map(|e| match e {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert_eq!(texts, vec!["Hello from mock", " world"]);

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage { input_tokens: 15, output_tokens: 4, .. })));
    }

    #[tokio::test]
    async fn stream_auth_failure_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = ChatCompletionsProvider::new(
            server.uri(),
            "bad-key".into(),
            CompatFlags::default(),
        );
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 100,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };

        let result = provider.stream(params).await;
        assert!(matches!(result, Err(crate::error::ProviderError::AuthFailed)));
    }

    #[tokio::test]
    async fn stream_tool_call_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};
        use tokio_stream::StreamExt;

        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"/tmp\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = ChatCompletionsProvider::new(
            server.uri(),
            "test-key".into(),
            CompatFlags::default(),
        );
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 100,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };

        let Ok(stream) = provider.stream(params).await else {
            panic!("stream should succeed");
        };
        tokio::pin!(stream);
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            let Ok(event) = item else { panic!("event should be Ok") };
            events.push(event);
        }

        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallStart { tool_name, .. } if tool_name == "read_file")));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { call_id, arguments }
            if call_id == "call_1" && arguments.contains("/tmp"))));
    }

    #[test]
    fn convert_tool_none_input_schema_uses_fallback() {
        let tool = ToolDefinition {
            name: "my_tool".into(),
            description: "a tool".into(),
            input_schema: None,
        };
        let json = convert_tool(&tool);
        let params = &json["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert!(params["properties"].is_object());
    }

    #[test]
    fn convert_tool_missing_type_key_injects_object() {
        let tool = ToolDefinition {
            name: "my_tool".into(),
            description: "a tool".into(),
            input_schema: Some(serde_json::json!({"properties": {"x": {"type": "string"}}})),
        };
        let json = convert_tool(&tool);
        let params = &json["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["x"]["type"], "string");
    }

    #[test]
    fn convert_message_mixed_text_and_tool_result_filters_text() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text { text: "some context".into() },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "result".into(),
                    is_error: false,
                },
            ],
        };
        let messages = convert_message(&msg);
        // Text emitted as preceding user message, then tool result
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "some context");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn parse_openai_sse_done_drains_pending_tool_calls() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"/tmp\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallStart { .. })));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallDelta { .. })));
        let tool_end = events.iter().find(|e| matches!(e, StreamEvent::ToolCallEnd { .. }));
        assert!(
            matches!(tool_end, Some(StreamEvent::ToolCallEnd { call_id, arguments })
                if call_id == "call_1" && arguments.contains("/tmp")),
            "pending tool call should be drained with accumulated arguments"
        );
    }

    #[tokio::test]
    async fn parse_openai_sse_finish_reason_length_emits_error() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "partial")));
        let error = events.iter().find(|e| matches!(e, StreamEvent::Error { .. }));
        assert!(matches!(error, Some(StreamEvent::Error { code, .. }) if code == "max_tokens"));
    }

    #[tokio::test]
    async fn parse_openai_sse_finish_reason_length_during_tool_call() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"test\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        // Error from "length" finish_reason
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Error { code, .. } if code == "max_tokens")));
        // Truncated tool calls must NOT be emitted
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })));
    }

    #[tokio::test]
    async fn parse_openai_sse_finish_reason_content_filter_emits_error() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "partial")));
        let error = events.iter().find(|e| matches!(e, StreamEvent::Error { .. }));
        assert!(matches!(error, Some(StreamEvent::Error { code, .. }) if code == "content_filter"));
    }

    #[tokio::test]
    async fn parse_openai_sse_mid_stream_error_object() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"error\":{\"message\":\"overloaded\",\"code\":\"overloaded_error\"}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "partial")));
        assert!(events.iter().any(
            |e| matches!(e, StreamEvent::Error { message, code, fatal: true } if message == "overloaded" && code == "overloaded_error")
        ));
    }

    #[test]
    fn build_body_reasoning_no_temperature() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        // build_params strips temperature when reasoning is active
        let params = RequestParams {
            model: "o3-mini",
            max_tokens: 2048,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: Some(crate::model::ReasoningLevel::Medium),
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert!(body.get("temperature").is_none());
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["max_completion_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_body_skip_stream_options_omits_stream_options() {
        let provider = ChatCompletionsProvider::new(
            "https://api.example.com".into(),
            "test-key".into(),
            CompatFlags {
                skip_stream_options: true,
                ..CompatFlags::default()
            },
        );
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert!(
            body.get("stream_options").is_none(),
            "stream_options should be omitted when skip_stream_options is true"
        );
    }

    #[test]
    fn build_body_no_tools_with_explicit_choice_flag_omits_tool_choice() {
        let provider = ChatCompletionsProvider::new(
            "https://api.example.com".into(),
            "test-key".into(),
            CompatFlags {
                explicit_tool_choice_auto: true,
                ..CompatFlags::default()
            },
        );
        let (msgs, tools) = minimal_params(); // empty tools
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        // tool_choice should NOT be present when tools is empty
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("tools").is_none());
    }

    #[tokio::test]
    async fn parse_openai_sse_empty_tool_name_rejected() {
        use tokio_stream::StreamExt;
        let stream = parse_openai_sse(byte_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()), "empty tool name should produce error");
    }

    #[tokio::test]
    async fn parse_openai_sse_missing_tool_name_rejected() {
        use tokio_stream::StreamExt;
        let stream = parse_openai_sse(byte_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()), "missing tool name should produce error");
    }

    #[tokio::test]
    async fn parse_openai_sse_tool_call_missing_id_rejected() {
        use tokio_stream::StreamExt;
        let stream = parse_openai_sse(byte_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read_file\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: [DONE]\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()), "tool call without id should produce error");
    }

    #[test]
    fn build_body_with_reasoning_and_tools() {
        let provider = make_provider();
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: Some(serde_json::json!({"type": "object"})),
        }];
        // build_params strips temperature when reasoning is active
        let params = RequestParams {
            model: "o3-mini",
            max_tokens: 2048,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: Some(crate::model::ReasoningLevel::Medium),
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["max_completion_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn validate_params_rejects_tools_and_output_schema() {
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: None,
        }];
        let schema = serde_json::json!({"type": "object"});
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: Some(&schema),
        };
        let result = validate_params(&params);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_openai_sse_tool_call_delta_no_id_after_start_ok() {
        let events = collect_openai_events(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\": \\\"/tmp\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).await;
        assert!(matches!(&events[0], StreamEvent::ToolCallStart { call_id, .. } if call_id == "call_1"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { call_id, .. } if call_id == "call_1"));
        assert!(matches!(&events[2], StreamEvent::ToolCallEnd { call_id, .. } if call_id == "call_1"));
    }

    #[tokio::test]
    async fn stream_rejects_non_sse_content_type() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("{\"error\": \"not sse\"}", "application/json"),
            )
            .mount(&server)
            .await;

        let provider = ChatCompletionsProvider::new(
            server.uri(),
            "test-key".into(),
            CompatFlags::default(),
        );
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "test-model",
            max_tokens: 100,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };

        let result = provider.stream(params).await;
        match result {
            Err(crate::error::ProviderError::SseParse(msg)) => {
                assert!(msg.contains("application/json"), "expected application/json in message, got: {msg}");
            }
            Ok(_) => panic!("expected SseParse error, got Ok"),
            Err(e) => panic!("expected SseParse, got {e:?}"),
        }
    }
}
