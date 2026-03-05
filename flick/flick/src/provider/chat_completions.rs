use reqwest::Client;

use crate::config::CompatFlags;
use crate::context::{ContentBlock, Message, Role};
use crate::error::ProviderError;
use crate::model::openai_reasoning_effort;
use std::pin::Pin;

use crate::provider::{
    DynProvider, ModelResponse, RequestParams, ToolCallResponse, ToolDefinition, UsageResponse,
};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

pub struct ChatCompletionsProvider {
    base_url: String,
    api_key: String,
    compat: CompatFlags,
    client: Client,
}

impl ChatCompletionsProvider {
    pub fn new(base_url: &str, api_key: String, compat: CompatFlags, client: Client) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            compat,
            client,
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
        });

        // Chat Completions: if max_tokens is None, omit entirely (API uses model default).
        // If Some, reasoning models use max_completion_tokens, others use max_tokens.
        if let Some(max) = params.max_tokens {
            if params.reasoning.is_some() {
                body["max_completion_tokens"] = serde_json::json!(max);
            } else {
                body["max_tokens"] = serde_json::json!(max);
            }
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

impl DynProvider for ChatCompletionsProvider {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        Box::pin(async move {
            if params.messages.is_empty() {
                return Err(ProviderError::ResponseParse("messages array is empty".into()));
            }
            let body = self.build_body(&params);
            let url = format!("{}/v1/chat/completions", self.base_url);

            let json = super::http::request_json(|| {
                self.client
                    .post(&url)
                    .header("authorization", format!("Bearer {}", self.api_key))
                    .json(&body)
            })
            .await?;

            parse_response(&json)
        })
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        if params.messages.is_empty() {
            return Err(ProviderError::ResponseParse("messages array is empty".into()));
        }
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
                let output = if *is_error && !content.starts_with("Error:") {
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

    // Chat Completions only supports tool_calls on assistant messages. If a user
    // message somehow contains ToolUse blocks, they will be silently dropped by
    // the text-only path below. Catch this upstream bug during development.
    debug_assert!(
        !(role == "user" && has_tool_use),
        "user message contains ToolUse blocks which cannot be represented in Chat Completions"
    );

    let text: String = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    // For assistant messages, if text is empty (e.g. only Thinking blocks),
    // skip emitting the message entirely to avoid empty content in the context window.
    if text.is_empty() && role == "assistant" {
        return vec![];
    }

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
    // OpenAI strict mode requires "additionalProperties": false on ALL object schemas
    enforce_no_additional_properties(&mut params);
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": params,
            "strict": true,
        }
    })
}

/// Recursively walk a JSON schema value and ensure every object-typed schema
/// has `"additionalProperties": false`, as required by OpenAI strict mode.
fn enforce_no_additional_properties(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // If this schema node is type "object", force additionalProperties to false.
    if obj.get("type").and_then(|t| t.as_str()) == Some("object") {
        obj.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    }

    // Recurse into each value under "properties".
    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (_key, prop_schema) in props.iter_mut() {
            enforce_no_additional_properties(prop_schema);
        }
    }

    // Recurse into "items" (for array types).
    if let Some(items) = obj.get_mut("items") {
        enforce_no_additional_properties(items);
    }
}

fn parse_response(json: &serde_json::Value) -> Result<ModelResponse, ProviderError> {
    let mut text = None;
    let mut tool_calls = Vec::new();

    // Extract from choices[0].message
    if let Some(choices) = json["choices"].as_array() {
        if let Some(choice) = choices.first() {
            let message = &choice["message"];

            // Text content
            if let Some(content) = message["content"].as_str() {
                if !content.is_empty() {
                    text = Some(content.to_string());
                }
            }

            // Tool calls
            if let Some(tcs) = message["tool_calls"].as_array() {
                for tc in tcs {
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    let name = tc["function"]["name"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::ResponseParse("tool call missing function name".into())
                        })?
                        .to_string();
                    let arguments = tc["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string();
                    tool_calls.push(ToolCallResponse {
                        call_id: id,
                        tool_name: name,
                        arguments,
                    });
                }
            }

            // Refusal: OpenAI models may set a `refusal` field instead of content
            if text.is_none() && tool_calls.is_empty() {
                if let Some(refusal) = message["refusal"].as_str() {
                    if !refusal.is_empty() {
                        return Err(ProviderError::ResponseParse(
                            format!("model refused: {refusal}"),
                        ));
                    }
                }
            }
        }
    }

    // Usage
    let usage_obj = &json["usage"];
    let (input_tokens, output_tokens) = extract_token_pair(
        usage_obj,
        "prompt_tokens",
        "completion_tokens",
    )
    .unwrap_or((0, 0));
    let cached = usage_obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    Ok(ModelResponse {
        text,
        thinking: Vec::new(), // OpenAI does not expose thinking blocks
        tool_calls,
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: cached,
        },
    })
}

/// Extract an (`input_tokens`, `output_tokens`) pair from a JSON usage object.
/// Returns `None` if neither field is present.
fn extract_token_pair(
    usage: &serde_json::Value,
    input_field: &str,
    output_field: &str,
) -> Option<(u64, u64)> {
    let input = usage.get(input_field).and_then(serde_json::Value::as_u64);
    let output = usage.get(output_field).and_then(serde_json::Value::as_u64);
    if input.is_some() || output.is_some() {
        Some((input.unwrap_or(0), output.unwrap_or(0)))
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
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

    use super::super::test_helpers::minimal_params;

    fn make_provider() -> ChatCompletionsProvider {
        ChatCompletionsProvider::new(
            "https://api.example.com",
            "test-key".into(),
            CompatFlags::default(),
            Client::new(),
        )
    }

    #[test]
    fn build_body_minimal() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: Some(1024),
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
        assert!(body.get("temperature").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_none_max_tokens_omits_field() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn build_body_with_system_prompt() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: Some(1024),
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
            "https://api.example.com",
            "test-key".into(),
            CompatFlags {
                explicit_tool_choice_auto: true,
            },
            Client::new(),
        );
        let (msgs, _) = minimal_params();
        let tools = vec![crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: Some(serde_json::json!({"type": "object"})),
        }];
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: Some(1024),
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
            max_tokens: Some(1024),
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
            max_tokens: Some(1024),
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

    #[test]
    fn convert_message_tool_result_error_no_double_prefix() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_err2".into(),
                content: "Error: already prefixed".into(),
                is_error: true,
            }],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Error: already prefixed");
    }

    #[test]
    fn convert_message_thinking_only_skipped() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "let me think about this...".into(),
                signature: "sig".into(),
            }],
        };
        let messages = convert_message(&msg);
        assert!(messages.is_empty(), "thinking-only assistant message should produce empty vec");
    }

    #[test]
    fn convert_message_thinking_plus_text() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "let me think about this...".into(),
                    signature: "sig".into(),
                },
                ContentBlock::Text {
                    text: "Here is my answer".into(),
                },
            ],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "Here is my answer");
    }

    // -- parse_response tests --

    #[test]
    fn parse_response_text_only() {
        let json = serde_json::json!({
            "choices": [{"message": {"content": "Hello world"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50}
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.text.as_deref(), Some("Hello world"));
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 50);
    }

    #[test]
    fn parse_response_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"/tmp\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 30}
        });
        let resp = parse_response(&json).expect("should parse");
        assert!(resp.text.is_none());
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "call_1");
        assert_eq!(resp.tool_calls[0].tool_name, "read_file");
        assert_eq!(resp.tool_calls[0].arguments, r#"{"path":"/tmp"}"#);
    }

    #[test]
    fn parse_response_cached_tokens() {
        let json = serde_json::json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "prompt_tokens_details": {"cached_tokens": 40}
            }
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.usage.cache_read_input_tokens, 40);
    }

    #[test]
    fn parse_response_multiple_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [
                        {"id": "call_a", "function": {"name": "read_file", "arguments": "{}"}},
                        {"id": "call_b", "function": {"name": "write_file", "arguments": "{}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 50, "completion_tokens": 30}
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.tool_calls.len(), 2);
        assert_eq!(resp.tool_calls[0].call_id, "call_a");
        assert_eq!(resp.tool_calls[1].call_id, "call_b");
    }

    #[test]
    fn parse_response_refusal() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "I cannot help with that"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        match parse_response(&json) {
            Err(ProviderError::ResponseParse(msg)) => {
                assert!(
                    msg.contains("model refused: I cannot help with that"),
                    "unexpected error message: {msg}"
                );
            }
            Err(other) => panic!("expected ResponseParse, got: {other:?}"),
            Ok(_) => panic!("expected error for refusal, got Ok"),
        }
    }

    #[test]
    fn parse_response_refusal_with_content() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Here is your answer",
                    "refusal": "I cannot help with that"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let resp = parse_response(&json).expect("should succeed when content is present");
        assert_eq!(resp.text.as_deref(), Some("Here is your answer"));
    }

    #[test]
    fn parse_response_empty_refusal_is_not_error() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": ""
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let resp = parse_response(&json).expect("empty refusal should not be an error");
        assert!(resp.text.is_none());
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn parse_response_refusal_ignored_when_tool_calls_present() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "I cannot help with that",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let resp = parse_response(&json).expect("refusal should be ignored when tool_calls present");
        assert!(resp.text.is_none());
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].tool_name, "read_file");
    }

    #[test]
    fn convert_tool_includes_strict() {
        let tool = crate::provider::ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            })),
        };
        let result = convert_tool(&tool);
        assert_eq!(result["type"], "function");
        assert_eq!(
            result["function"]["strict"], true,
            "function definition should include strict: true"
        );
        assert_eq!(
            result["function"]["parameters"]["additionalProperties"], false,
            "parameters should include additionalProperties: false"
        );
    }

    #[test]
    fn convert_tool_strict_overrides_additional_properties() {
        let tool = crate::provider::ToolDefinition {
            name: "test".into(),
            description: "Test".into(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            })),
        };
        let result = convert_tool(&tool);
        // Strict mode requires additionalProperties: false — even if explicitly set to true
        assert_eq!(
            result["function"]["parameters"]["additionalProperties"], false,
            "strict mode must override additionalProperties to false"
        );
    }

    #[test]
    fn convert_tool_nested_objects_get_additional_properties_false() {
        let tool = crate::provider::ToolDefinition {
            name: "test".into(),
            description: "Test".into(),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": {
                            "deep": {
                                "type": "object",
                                "properties": {}
                            }
                        }
                    },
                    "list": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {}
                        }
                    }
                }
            })),
        };
        let result = convert_tool(&tool);
        let params = &result["function"]["parameters"];
        // Top level
        assert_eq!(
            params["additionalProperties"], false,
            "top-level object must have additionalProperties: false"
        );
        // Nested object property
        assert_eq!(
            params["properties"]["nested"]["additionalProperties"], false,
            "nested object must have additionalProperties: false"
        );
        // Deeply nested object property
        assert_eq!(
            params["properties"]["nested"]["properties"]["deep"]["additionalProperties"], false,
            "deeply nested object must have additionalProperties: false"
        );
        // Object inside array items
        assert_eq!(
            params["properties"]["list"]["items"]["additionalProperties"], false,
            "object in array items must have additionalProperties: false"
        );
    }

    #[test]
    fn convert_tool_no_input_schema_gets_strict_defaults() {
        let tool = crate::provider::ToolDefinition {
            name: "noop".into(),
            description: "No-op tool".into(),
            input_schema: None,
        };
        let result = convert_tool(&tool);
        assert_eq!(result["function"]["strict"], true);
        assert_eq!(result["function"]["parameters"]["type"], "object");
        assert_eq!(result["function"]["parameters"]["properties"], serde_json::json!({}));
        assert_eq!(result["function"]["parameters"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn call_boxed_rejects_empty_messages() {
        let provider = make_provider();
        let msgs: Vec<Message> = vec![];
        let tools = vec![];
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: Some(1024),
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let result = provider.call_boxed(params).await;
        match result {
            Err(ProviderError::ResponseParse(msg)) => {
                assert_eq!(msg, "messages array is empty");
            }
            Err(other) => panic!("expected ResponseParse error, got: {other:?}"),
            Ok(_) => panic!("expected error for empty messages, got Ok"),
        }
    }

    #[test]
    fn build_request_rejects_empty_messages() {
        let provider = make_provider();
        let msgs: Vec<Message> = vec![];
        let tools = vec![];
        let params = RequestParams {
            model: "gpt-4o",
            max_tokens: Some(1024),
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let result = provider.build_request(params);
        match result {
            Err(ProviderError::ResponseParse(msg)) => {
                assert_eq!(msg, "messages array is empty");
            }
            Err(other) => panic!("expected ResponseParse error, got: {other:?}"),
            Ok(_) => panic!("expected error for empty messages, got Ok"),
        }
    }

    /// In debug builds, a User message with ToolUse blocks triggers the debug_assert.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "user message contains ToolUse blocks")]
    fn convert_message_user_with_tool_use_panics_in_debug() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "some text".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_bad".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp"}),
                },
            ],
        };
        let _ = convert_message(&msg);
    }

    /// In release builds, a User message with ToolUse blocks should have
    /// the ToolUse blocks silently dropped, preserving only the text.
    /// In debug builds the debug_assert fires, so we only run the body
    /// when debug_assertions are off.
    #[test]
    #[cfg(not(debug_assertions))]
    fn convert_message_user_with_tool_use_drops_blocks() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "some text".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_bad".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp"}),
                },
            ],
        };
        let messages = convert_message(&msg);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "some text");
        assert!(
            messages[0].get("tool_calls").is_none(),
            "user messages must not have tool_calls"
        );
    }

}
