use std::time::Duration;

use reqwest::Client;

use crate::context::{ContentBlock, Message, Role};
use crate::error::ProviderError;
use crate::model::anthropic_budget_tokens;
use crate::provider::{
    ModelResponse, Provider, RequestParams, ThinkingContent, ToolCallResponse, UsageResponse,
};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

pub struct MessagesProvider {
    base_url: String,
    api_key: String,
    client: Client,
}

impl MessagesProvider {
    #[allow(clippy::expect_used)] // Client::new() panics on same failure
    pub fn new(base_url: &str, api_key: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
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
        // Messages API always requires max_tokens.
        // Resolve: explicit → registry → 8192.
        let resolved_max = params.max_tokens
            .or_else(|| crate::model::default_max_output_tokens(params.model))
            .unwrap_or(8192);
        let mut body = serde_json::json!({
            "model": params.model,
            "max_tokens": resolved_max,
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
            let budget = anthropic_budget_tokens(level).min(resolved_max.saturating_sub(1));
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
    async fn call(
        &self,
        params: RequestParams<'_>,
    ) -> Result<ModelResponse, ProviderError> {
        let body = self.build_body(&params);
        let url = format!("{}/v1/messages", self.base_url);

        let json = super::http::request_json(|| {
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
        })
        .await?;

        parse_response(&json)
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(self.build_body(&params))
    }
}

fn parse_response(json: &serde_json::Value) -> Result<ModelResponse, ProviderError> {
    let mut text = String::new();
    let mut thinking = Vec::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = json["content"].as_array() {
        for block in content {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        text.push_str(t);
                    }
                }
                Some("thinking") => {
                    let t = block["thinking"].as_str().unwrap_or("").to_string();
                    let sig = block["signature"].as_str().unwrap_or("").to_string();
                    thinking.push(ThinkingContent {
                        text: t,
                        signature: sig,
                    });
                }
                Some("tool_use") => {
                    let id = block["id"]
                        .as_str()
                        .ok_or_else(|| ProviderError::ResponseParse("tool_use missing id".into()))?
                        .to_string();
                    let name = block["name"]
                        .as_str()
                        .ok_or_else(|| {
                            ProviderError::ResponseParse("tool_use missing name".into())
                        })?
                        .to_string();
                    let input = &block["input"];
                    let arguments = serde_json::to_string(input).unwrap_or_default();
                    tool_calls.push(ToolCallResponse {
                        call_id: id,
                        tool_name: name,
                        arguments,
                    });
                }
                _ => {}
            }
        }
    }

    // Usage
    let usage_obj = &json["usage"];
    let input_tokens = usage_obj["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = usage_obj["output_tokens"].as_u64().unwrap_or(0);
    let cache_creation = usage_obj["cache_creation_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    let cache_read = usage_obj["cache_read_input_tokens"].as_u64().unwrap_or(0);

    Ok(ModelResponse {
        text: if text.is_empty() { None } else { Some(text) },
        thinking,
        tool_calls,
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        },
    })
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

    use super::super::test_helpers::minimal_params;

    fn make_provider() -> MessagesProvider {
        MessagesProvider::new(DEFAULT_BASE_URL, "test-key".into())
    }

    #[test]
    fn build_body_minimal() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: Some(1024),
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
        assert!(body.get("temperature").is_none());
        assert!(body.get("system").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_body_none_max_tokens_uses_registry_fallback() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        // Registry has 64000 for claude-sonnet-4
        assert_eq!(body["max_tokens"], 64_000);
    }

    #[test]
    fn build_body_none_max_tokens_unknown_model_uses_8192() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "unknown-model",
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            messages: &msgs,
            tools: &tools,
            reasoning: None,
            output_schema: None,
        };
        let body = provider.build_body(&params);
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn build_body_with_system_and_temperature() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: Some(2048),
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
        assert_eq!(body["tools"][0]["name"], "read_file");
    }

    #[test]
    fn build_body_with_reasoning_no_temperature() {
        let provider = make_provider();
        let (msgs, tools) = minimal_params();
        let params = crate::provider::RequestParams {
            model: "claude-sonnet-4-20250514",
            max_tokens: Some(1024),
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

    // -- parse_response tests --

    #[test]
    fn parse_response_text_only() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "Hello world"}],
            "usage": {"input_tokens": 100, "output_tokens": 50},
            "stop_reason": "end_turn"
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.text.as_deref(), Some("Hello world"));
        assert!(resp.tool_calls.is_empty());
        assert!(resp.thinking.is_empty());
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 50);
    }

    #[test]
    fn parse_response_tool_use() {
        let json = serde_json::json!({
            "content": [
                {"type": "text", "text": "I'll read that file."},
                {"type": "tool_use", "id": "tc_1", "name": "read_file", "input": {"path": "/tmp"}}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 30},
            "stop_reason": "tool_use"
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.text.as_deref(), Some("I'll read that file."));
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].call_id, "tc_1");
        assert_eq!(resp.tool_calls[0].tool_name, "read_file");
        assert!(resp.tool_calls[0].arguments.contains("/tmp"));
    }

    #[test]
    fn parse_response_thinking() {
        let json = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "Let me reason", "signature": "sig_abc"},
                {"type": "text", "text": "Answer"}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5},
            "stop_reason": "end_turn"
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.thinking.len(), 1);
        assert_eq!(resp.thinking[0].text, "Let me reason");
        assert_eq!(resp.thinking[0].signature, "sig_abc");
        assert_eq!(resp.text.as_deref(), Some("Answer"));
    }

    #[test]
    fn parse_response_cache_tokens() {
        let json = serde_json::json!({
            "content": [{"type": "text", "text": "hi"}],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 30,
                "cache_read_input_tokens": 20
            },
            "stop_reason": "end_turn"
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.usage.cache_creation_input_tokens, 30);
        assert_eq!(resp.usage.cache_read_input_tokens, 20);
    }

    #[test]
    fn parse_response_multiple_tool_calls() {
        let json = serde_json::json!({
            "content": [
                {"type": "tool_use", "id": "tc_1", "name": "read_file", "input": {"path": "/a"}},
                {"type": "tool_use", "id": "tc_2", "name": "write_file", "input": {"path": "/b"}}
            ],
            "usage": {"input_tokens": 50, "output_tokens": 30},
            "stop_reason": "tool_use"
        });
        let resp = parse_response(&json).expect("should parse");
        assert_eq!(resp.tool_calls.len(), 2);
        assert_eq!(resp.tool_calls[0].call_id, "tc_1");
        assert_eq!(resp.tool_calls[1].call_id, "tc_2");
    }

    #[test]
    fn parse_response_empty_content() {
        let json = serde_json::json!({
            "content": [],
            "usage": {"input_tokens": 5, "output_tokens": 0},
            "stop_reason": "end_turn"
        });
        let resp = parse_response(&json).expect("should parse");
        assert!(resp.text.is_none());
        assert!(resp.tool_calls.is_empty());
    }
}
