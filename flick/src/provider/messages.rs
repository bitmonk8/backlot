use std::collections::HashMap;
use std::time::Duration;

use reqwest::Client;
use smallvec::smallvec;

use crate::context::{ContentBlock, Message, Role};
use crate::error::ProviderError;
use crate::event::StreamEvent;
use crate::model::anthropic_budget_tokens;
use crate::provider::sse::{self, EventBatch, SseAction};
use crate::provider::{EventStream, Provider, RequestParams};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

pub struct MessagesProvider {
    base_url: String,
    api_key: String,
    client: Client,
}

impl MessagesProvider {
    #[allow(clippy::expect_used)] // Client::new() panics on same failure
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
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

    #[allow(clippy::unused_self)]
    fn build_body(&self, params: &RequestParams<'_>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": params.model,
            "max_tokens": params.max_tokens,
            "stream": true,
        });

        if let Some(temp) = params.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(system) = params.system_prompt {
            body["system"] = serde_json::json!(system);
        }

        let messages: Vec<serde_json::Value> = params
            .messages
            .iter()
            .map(convert_message)
            .collect();
        body["messages"] = serde_json::Value::Array(messages);

        if !params.tools.is_empty() {
            let tools: Vec<serde_json::Value> = params
                .tools
                .iter()
                .map(|t| {
                    let mut tool = serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                    });
                    if let Some(schema) = &t.input_schema {
                        tool["input_schema"] = schema.clone();
                    } else {
                        tool["input_schema"] = serde_json::json!({"type": "object"});
                    }
                    tool
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(level) = params.reasoning {
            let budget = anthropic_budget_tokens(level).min(params.max_tokens.saturating_sub(1));
            if budget > 0 {
                body["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
            }
        }

        // Anthropic API does not support output_schema at the top level.
        // Structured output should use forced tool choice pattern instead.

        body
    }
}

impl Provider for MessagesProvider {
    async fn stream(
        &self,
        params: RequestParams<'_>,
    ) -> Result<EventStream, ProviderError> {
        let body = self.build_body(&params);
        let url = format!("{}/v1/messages", self.base_url);

        sse::stream_request(
            || {
                self.client
                    .post(&url)
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", API_VERSION)
                    .header("content-type", "application/json")
                    .json(&body)
            },
            parse_sse_stream,
        )
        .await
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(self.build_body(&params))
    }
}

fn convert_message(msg: &Message) -> serde_json::Value {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<serde_json::Value> = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => {
                Some(serde_json::json!({"type": "text", "text": text}))
            }
            // Omit thinking blocks with empty signature — unsigned thinking is
            // invalid for round-tripping and would cause an API validation error.
            ContentBlock::Thinking { signature, .. } if signature.is_empty() => None,
            ContentBlock::Thinking { text, signature } => {
                Some(serde_json::json!({"type": "thinking", "thinking": text, "signature": signature}))
            }
            ContentBlock::ToolUse { id, name, input } => {
                Some(serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                Some(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                    "is_error": is_error,
                }))
            }
        })
        .collect();
    serde_json::json!({"role": role, "content": content})
}

/// Emits a Usage event whenever the usage object contains token fields,
/// including zero values (Anthropic legitimately sends `output_tokens`: 0
/// in `message_start`).
fn extract_usage(usage: &serde_json::Value) -> Option<StreamEvent> {
    let (input_tokens, output_tokens) =
        crate::provider::extract_token_pair(usage, "input_tokens", "output_tokens")?;
    Some(StreamEvent::Usage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    })
}

/// Per-block-index tracking for tool calls.
struct BlockState {
    tool_call_id: String,
    accumulated_input: String,
}

#[allow(clippy::too_many_lines)]
fn parse_sse_stream(
    byte_stream: impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
        + Send
        + 'static,
) -> impl tokio_stream::Stream<Item = Result<StreamEvent, ProviderError>> + Send {
    // Track tool calls by block index
    let mut block_states: HashMap<u64, BlockState> = HashMap::new();

    sse::spawn_sse_parser(byte_stream, sse::DEFAULT_SSE_IDLE_TIMEOUT, move |block: &str| {
        let (event_type, data) = sse::parse_event_data(block);
        let (Some(event_type), Some(data)) = (event_type, data) else {
            return Ok(SseAction::Events(smallvec![]));
        };

        let parsed: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::SseParse(format!("{event_type}: {e}")))?;

        let events = match event_type {
            "content_block_start" => {
                let index = parsed["index"].as_u64().ok_or_else(|| {
                    ProviderError::SseParse("content_block_start missing index".into())
                })?;
                let content_block = &parsed["content_block"];
                if content_block["type"].as_str() == Some("tool_use") {
                    let id = content_block["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ProviderError::SseParse("tool_use block missing id".into()))?
                        .to_string();
                    let tool_name = content_block["name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| ProviderError::SseParse("tool_use block missing name".into()))?
                        .to_string();
                    block_states.insert(
                        index,
                        BlockState {
                            tool_call_id: id.clone(),
                            accumulated_input: String::new(),
                        },
                    );
                    smallvec![StreamEvent::ToolCallStart {
                        call_id: id,
                        tool_name,
                    }]
                } else {
                    smallvec![]
                }
            }
            "content_block_delta" => {
                let index = parsed["index"].as_u64().ok_or_else(|| {
                    ProviderError::SseParse("content_block_delta missing index".into())
                })?;
                let delta = &parsed["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("").to_string();
                        smallvec![StreamEvent::TextDelta { text }]
                    }
                    Some("thinking_delta") => {
                        let text = delta["thinking"].as_str().unwrap_or("").to_string();
                        smallvec![StreamEvent::ThinkingDelta { text }]
                    }
                    Some("signature_delta") => {
                        let sig = delta["signature"].as_str().unwrap_or("").to_string();
                        smallvec![StreamEvent::ThinkingSignature { signature: sig }]
                    }
                    Some("input_json_delta") => {
                        let partial =
                            delta["partial_json"].as_str().unwrap_or("").to_string();
                        if let Some(state) = block_states.get_mut(&index) {
                            state.accumulated_input.push_str(&partial);
                            smallvec![StreamEvent::ToolCallDelta {
                                call_id: state.tool_call_id.clone(),
                                arguments_delta: partial,
                            }]
                        } else {
                            smallvec![]
                        }
                    }
                    _ => smallvec![],
                }
            }
            "content_block_stop" => {
                let index = parsed["index"].as_u64().ok_or_else(|| {
                    ProviderError::SseParse("content_block_stop missing index".into())
                })?;
                if let Some(state) = block_states.remove(&index) {
                    smallvec![StreamEvent::ToolCallEnd {
                        call_id: state.tool_call_id,
                        arguments: state.accumulated_input,
                    }]
                } else {
                    smallvec![]
                }
            }
            "message_delta" => {
                let mut events: EventBatch =
                    extract_usage(&parsed["usage"]).into_iter().collect();
                // Surface truncation when stop_reason is "max_tokens".
                // Non-fatal: the response received so far is valid.
                if parsed["delta"]["stop_reason"].as_str() == Some("max_tokens") {
                    events.push(StreamEvent::Error {
                        message: "model response truncated (max tokens exceeded)".into(),
                        code: "max_tokens".into(),
                        fatal: false,
                    });
                }
                events
            }
            "message_start" => extract_usage(&parsed["message"]["usage"]).into_iter().collect(),
            "message_stop" => return Ok(SseAction::Done),
            // Surface Anthropic mid-stream error events.
            // Anthropic terminates the stream after an error event, so we use
            // DoneWithEvents to emit the error and close the stream immediately
            // rather than waiting for the server to drop the connection.
            "error" => {
                let message = parsed["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                let code = parsed["error"]["type"]
                    .as_str()
                    .unwrap_or("error")
                    .to_string();
                return Ok(SseAction::DoneWithEvents(
                    smallvec![StreamEvent::Error { message, code, fatal: true }],
                ));
            }
            _ => smallvec![],
        };

        Ok(SseAction::Events(events))
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::context::{ContentBlock, Message, Role};

    #[test]
    fn convert_message_text() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "hello");
    }

    #[test]
    fn convert_message_thinking_with_signature() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "reasoning here".into(),
                signature: "sig_abc123".into(),
            }],
        };
        let json = convert_message(&msg);
        assert_eq!(json["content"][0]["type"], "thinking");
        assert_eq!(json["content"][0]["thinking"], "reasoning here");
        assert_eq!(json["content"][0]["signature"], "sig_abc123");
    }

    #[test]
    fn convert_message_thinking_without_signature_omitted() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "reasoning".into(),
                signature: String::new(),
            }],
        };
        let json = convert_message(&msg);
        // Unsigned thinking blocks are invalid for round-tripping and must be omitted
        assert_eq!(json["content"].as_array().expect("content array").len(), 0);
    }

    #[test]
    fn convert_message_tool_use() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            }],
        };
        let json = convert_message(&msg);
        assert_eq!(json["content"][0]["type"], "tool_use");
        assert_eq!(json["content"][0]["id"], "call_1");
        assert_eq!(json["content"][0]["name"], "read_file");
    }

    #[test]
    fn convert_message_tool_result() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "file contents".into(),
                is_error: false,
            }],
        };
        let json = convert_message(&msg);
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["tool_use_id"], "call_1");
        assert_eq!(json["content"][0]["is_error"], false);
    }

    #[test]
    fn extract_usage_with_tokens() {
        let usage = serde_json::json!({"input_tokens": 100, "output_tokens": 50});
        let event = extract_usage(&usage);
        match event {
            Some(StreamEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            }) => {
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 50);
            }
            other => panic!("expected Some(Usage), got {other:?}"),
        }
    }

    #[test]
    fn extract_usage_zero_tokens() {
        let usage = serde_json::json!({"input_tokens": 0, "output_tokens": 0});
        // Zero-token usage events are now emitted (Anthropic sends output_tokens: 0
        // in message_start legitimately)
        assert!(extract_usage(&usage).is_some());
    }

    #[test]
    fn extract_usage_no_token_fields() {
        let usage = serde_json::json!({});
        assert!(extract_usage(&usage).is_none());
    }

    #[test]
    fn extract_usage_with_cache_tokens() {
        let usage = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_creation_input_tokens": 30,
            "cache_read_input_tokens": 20
        });
        match extract_usage(&usage) {
            Some(StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            }) => {
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 50);
                assert_eq!(cache_creation_input_tokens, 30);
                assert_eq!(cache_read_input_tokens, 20);
            }
            other => panic!("expected Some(Usage), got {other:?}"),
        }
    }

    use super::super::test_helpers::{byte_stream, minimal_params};

    fn make_provider() -> MessagesProvider {
        MessagesProvider::new(DEFAULT_BASE_URL.to_string(), "test-key".into())
    }

    #[test]
    fn build_body_minimal() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stream"], true);
        assert!(body.get("temperature").is_none());
        assert!(body.get("system").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_body_with_system_and_temperature() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: 2048,
            temperature: Some(0.5),
            system_prompt: Some("Be helpful"),
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["system"], "Be helpful");
    }

    #[test]
    fn build_body_with_tools() {
        let provider = make_provider();
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: Some(serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}})),
        }];
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
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
        assert_eq!(body["tools"][0]["name"], "read_file");
    }

    #[test]
    fn build_body_with_reasoning_no_temperature() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        // build_params strips temperature when reasoning is active
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: Some(crate::model::ReasoningLevel::High),
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert!(body.get("temperature").is_none());
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 1023);
    }

    // -- parse_sse_stream byte-stream tests -----------------------------------

    async fn collect_anthropic_events(chunks: Vec<&str>) -> Vec<StreamEvent> {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(chunks));
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
    async fn parse_sse_stream_text_deltas() {
        let events = collect_anthropic_events(vec![
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
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
    async fn parse_sse_stream_thinking_and_signature() {
        let events = collect_anthropic_events(vec![
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_abc\"}}\n\n",
        ]).await;

        assert!(matches!(&events[0], StreamEvent::ThinkingDelta { text } if text == "hmm"));
        assert!(matches!(&events[1], StreamEvent::ThinkingSignature { signature } if signature == "sig_abc"));
    }

    #[tokio::test]
    async fn parse_sse_stream_tool_call_sequence() {
        let events = collect_anthropic_events(vec![
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc_1\",\"name\":\"read_file\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"/tmp\\\"\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
        ]).await;

        assert!(matches!(&events[0], StreamEvent::ToolCallStart { call_id, tool_name }
            if call_id == "tc_1" && tool_name == "read_file"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { call_id, .. }
            if call_id == "tc_1"));
        assert!(matches!(&events[2], StreamEvent::ToolCallDelta { call_id, .. }
            if call_id == "tc_1"));
        assert!(matches!(&events[3], StreamEvent::ToolCallEnd { call_id, arguments }
            if call_id == "tc_1" && arguments == "{\"path\": \"/tmp\""));
    }

    #[tokio::test]
    async fn parse_sse_stream_usage_from_message_start_and_delta() {
        let events = collect_anthropic_events(vec![
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":0}}}\n\n",
            "event: message_delta\ndata: {\"usage\":{\"input_tokens\":0,\"output_tokens\":50}}\n\n",
        ]).await;

        let usage_events: Vec<&StreamEvent> = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Usage { .. }))
            .collect();
        assert_eq!(usage_events.len(), 2);
        assert!(matches!(usage_events[0], StreamEvent::Usage { input_tokens: 100, output_tokens: 0, .. }));
        assert!(matches!(usage_events[1], StreamEvent::Usage { input_tokens: 0, output_tokens: 50, .. }));
    }

    #[tokio::test]
    async fn parse_sse_stream_parallel_tool_calls() {
        let events = collect_anthropic_events(vec![
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc_1\",\"name\":\"read_file\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc_2\",\"name\":\"write_file\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/a\\\"}\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/b\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
        ]).await;

        assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::ToolCallStart { .. })).count(), 2);
        let ends: Vec<_> = events.iter().filter_map(|e| {
            if let StreamEvent::ToolCallEnd { call_id, arguments } = e {
                Some((call_id.as_str(), arguments.as_str()))
            } else { None }
        }).collect();
        assert_eq!(ends.len(), 2);
        assert_eq!(ends[0].0, "tc_1");
        assert!(ends[0].1.contains("/a"));
        assert_eq!(ends[1].0, "tc_2");
        assert!(ends[1].1.contains("/b"));
    }

    #[tokio::test]
    async fn parse_sse_stream_message_stop() {
        let events = collect_anthropic_events(vec![
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"before\"}}\n\n",
            "event: message_stop\ndata: {}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"after\"}}\n\n",
        ]).await;
        // "after" should not appear because message_stop returns Done
        let texts: Vec<&str> = events.iter().filter_map(|e| match e {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert_eq!(texts, vec!["before"]);
    }

    #[tokio::test]
    async fn parse_sse_stream_missing_index_field_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"no index\"}}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()));
    }

    #[tokio::test]
    async fn parse_sse_stream_missing_index_on_block_start_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_start\ndata: {\"content_block\":{\"type\":\"text\"}}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()));
    }

    #[tokio::test]
    async fn parse_sse_stream_missing_index_on_block_stop_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_stop\ndata: {}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()));
    }

    #[tokio::test]
    async fn parse_sse_stream_malformed_json_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_delta\ndata: {not valid json}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some());
        assert!(first.as_ref().is_some_and(Result::is_err));
    }

    #[tokio::test]
    async fn parse_sse_stream_chunks_split_mid_block() {
        // A single SSE block split across two byte chunks
        let events = collect_anthropic_events(vec![
            "event: content_block_delta\ndata: {\"index\":0,",
            "\"delta\":{\"type\":\"text_delta\",\"text\":\"split\"}}\n\n",
        ]).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "split"));
    }

    // -- wiremock-based Provider::stream tests --

    #[tokio::test]
    async fn stream_text_response_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path, header};
        use tokio_stream::StreamExt;

        let sse_body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from mock\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"usage\":{\"input_tokens\":0,\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = MessagesProvider::new(server.uri(), "test-key".into());
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
        assert_eq!(texts, vec!["Hello from mock"]);

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage { .. })));
    }

    #[tokio::test]
    async fn stream_auth_failure_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = MessagesProvider::new(server.uri(), "bad-key".into());
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
        assert!(matches!(result, Err(crate::error::ProviderError::AuthFailed)));
    }

    #[tokio::test]
    async fn stream_rate_limit_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("rate limited")
                    .append_header("retry-after", "0"),
            )
            .expect(4) // 1 initial + 3 retries
            .mount(&server)
            .await;

        let provider = MessagesProvider::new(server.uri(), "test-key".into());
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
            Err(crate::error::ProviderError::RateLimited { retry_after_ms }) => {
                assert_eq!(retry_after_ms, Some(0));
            }
            Ok(_) => panic!("expected RateLimited, got Ok"),
            Err(e) => panic!("expected RateLimited, got Err({e:?})"),
        }
    }

    #[tokio::test]
    async fn stream_tool_call_via_http() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};
        use tokio_stream::StreamExt;

        let sse_body = concat!(
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc_1\",\"name\":\"read_file\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/tmp\\\"}\" }}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_stop\ndata: {}\n\n",
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = MessagesProvider::new(server.uri(), "test-key".into());
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
        assert!(events.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { call_id, .. } if call_id == "tc_1")));
    }

    #[tokio::test]
    async fn content_block_start_non_tool_type_no_tool_call_start() {
        let events = collect_anthropic_events(vec![
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
        ]).await;
        // No ToolCallStart should be emitted for text blocks
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::ToolCallStart { .. })));
        // And no ToolCallEnd either (no state was inserted for this index)
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::ToolCallEnd { .. })));
    }

    #[tokio::test]
    async fn input_json_delta_unknown_block_index_ignored() {
        // input_json_delta at index 99 with no prior content_block_start for that index
        let events = collect_anthropic_events(vec![
            "event: content_block_delta\ndata: {\"index\":99,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"key\\\":\\\"val\\\"}\"}}\n\n",
        ]).await;
        // Should produce no events (no state for index 99)
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn tool_use_block_missing_id_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"name\":\"read_file\"}}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()));
    }

    #[tokio::test]
    async fn tool_use_block_empty_name_returns_error() {
        use tokio_stream::StreamExt;
        let stream = parse_sse_stream(byte_stream(vec![
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tc_1\",\"name\":\"\"}}\n\n",
        ]));
        tokio::pin!(stream);
        let first = stream.next().await;
        assert!(first.is_some_and(|r| r.is_err()));
    }

    #[test]
    fn build_body_with_none_input_schema_uses_fallback() {
        let provider = make_provider();
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "my_tool".into(),
            description: "a tool".into(),
            input_schema: None,
        }];
        let params = crate::provider::RequestParams {
            model: "test",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn build_body_output_schema_silently_ignored() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let schema = serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}});
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: 1024,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: Some(&schema),
        };
        let body = provider.build_body(&params);
        // Anthropic does not support output_schema at the top level
        assert!(body.get("output_schema").is_none(), "output_schema should not appear in Anthropic request body");
        assert!(body.get("response_format").is_none(), "response_format should not appear in Anthropic request body");
    }

    #[tokio::test]
    async fn parse_sse_stream_error_event() {
        // An error event should emit the error AND terminate the stream.
        // The text delta after the error must not appear.
        let events = collect_anthropic_events(vec![
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n\
             event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ghost\"}}\n\n",
        ]).await;

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Error { message, code, fatal: true }
            if message == "Overloaded" && code == "overloaded_error"
        ));
    }

    #[tokio::test]
    async fn parse_sse_stream_max_tokens_stop_reason() {
        let events = collect_anthropic_events(vec![
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"input_tokens\":0,\"output_tokens\":50}}\n\n",
        ]).await;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage { output_tokens: 50, .. })));
        let error = events.iter().find(|e| matches!(e, StreamEvent::Error { .. }));
        assert!(matches!(error, Some(StreamEvent::Error { code, .. }) if code == "max_tokens"));
    }

    #[tokio::test]
    async fn parse_sse_stream_end_turn_stop_reason_no_error() {
        let events = collect_anthropic_events(vec![
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":0,\"output_tokens\":50}}\n\n",
        ]).await;

        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage { .. })));
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error { .. })));
    }

    #[test]
    fn convert_message_mixed_content_types() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "let me think".into(),
                    signature: "sig_1".into(),
                },
                ContentBlock::Text {
                    text: "I'll help".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp"}),
                },
            ],
        };
        let json = convert_message(&msg);
        let content = json["content"].as_array().expect("content should be array");
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
    }

    #[tokio::test]
    async fn stream_rejects_non_sse_content_type() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("{\"error\": \"not sse\"}", "application/json"),
            )
            .mount(&server)
            .await;

        let provider = MessagesProvider::new(server.uri(), "test-key".into());
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
