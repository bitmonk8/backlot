#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;

use std::pin::Pin;

use flick::runner;
use flick::config::Config;
use flick::context::{ContentBlock, Context};
use flick::error::{FlickError, ProviderError};
use flick::provider::{DynProvider, ModelResponse, RequestParams, UsageResponse};
use flick::result::ResultStatus;

fn test_config() -> Config {
    Config::parse(
        r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#,
    )
    .expect("test config should parse")
}

fn test_config_with_tools() -> Config {
    Config::parse(
        r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"

[[tools]]
name = "read_file"
description = "Read a file"
parameters = { type = "object", properties = { path = { type = "string" } }, required = ["path"] }

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#,
    )
    .expect("test config should parse")
}

/// Single call returning Complete — model returns text, no tool_use.
#[tokio::test]
async fn run_single_call_complete() {
    let provider = MockProvider::new(vec![text_response("done", 100, 50)]);
    let config = test_config();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    assert_eq!(result.status, ResultStatus::Complete);
    assert_eq!(result.content.len(), 1);
    assert!(matches!(&result.content[0], ContentBlock::Text { text } if text == "done"));

    assert!(result.context_hash.is_none(), "context_hash should be None (computed by main.rs, not runner::run)");

    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
}

/// Single call returning ToolCallsPending — model returns tool_use blocks.
#[tokio::test]
async fn run_single_call_tool_calls_pending() {
    let provider = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        200,
        80,
    )]);
    let config = test_config_with_tools();
    let mut context = Context::default();
    context.push_user_text("read the file").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let tool_uses: Vec<_> = result
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .collect();
    assert_eq!(tool_uses.len(), 1);
    assert!(matches!(
        tool_uses[0],
        ContentBlock::ToolUse { name, .. } if name == "read_file"
    ));
}

/// Provider error propagates as Err.
#[tokio::test]
async fn run_provider_error_propagates() {
    struct ErrorProvider;
    impl DynProvider for ErrorProvider {
        fn call_boxed<'a>(
            &'a self,
            _params: RequestParams<'a>,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a,
            >,
        > {
            Box::pin(async {
                Err(ProviderError::Api {
                    status: 500,
                    message: "simulated error".into(),
                })
            })
        }
        fn build_request(
            &self,
            _params: RequestParams<'_>,
        ) -> Result<serde_json::Value, ProviderError> {
            Ok(serde_json::json!({}))
        }
    }

    let config = test_config();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &ErrorProvider, &mut context).await;
    assert!(matches!(
        result,
        Err(FlickError::Provider(ProviderError::Api { status: 500, .. }))
    ));
}

/// build_params maps all config fields correctly.
#[test]
fn build_params_maps_config_fields() {
    let config = Config::parse(
        r#"
system_prompt = "Be helpful"

[model]
provider = "test"
name = "test-model-123"
max_tokens = 2048
temperature = 0.7
reasoning = {level = "high"}

[provider.test]
api = "chat_completions"

[output_schema]
schema = {"type" = "object"}
"#,
    )
    .expect("test config should parse");

    let messages = vec![flick::context::Message {
        role: flick::context::Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".into(),
        }],
    }];
    let tool_defs = vec![flick::provider::ToolDefinition {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: Some(serde_json::json!({"type": "object"})),
    }];

    let params = runner::build_params(&config, &messages, &tool_defs);

    assert_eq!(params.model, "test-model-123");
    assert_eq!(params.max_tokens, Some(2048));
    // Temperature stripped when reasoning is active
    assert_eq!(params.temperature, None);
    assert_eq!(params.system_prompt, Some("Be helpful"));
    assert_eq!(params.messages.len(), 1);
    assert_eq!(params.tools.len(), 1);
    assert_eq!(params.reasoning, Some(flick::model::ReasoningLevel::High));
    assert!(params.output_schema.is_some());
    assert_eq!(params.output_schema.unwrap()["type"], "object");
}

/// Cost is computed correctly in the usage summary.
#[tokio::test]
async fn run_cost_in_result() {
    let provider = MockProvider::new(vec![text_response("answer", 1000, 500)]);
    let config = test_config();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    let usage = result.usage.unwrap();
    let expected_cost = config.compute_cost(1000, 500);
    assert!(
        (usage.cost_usd - expected_cost).abs() < 1e-10,
        "cost_usd ({}) should match compute_cost ({})",
        usage.cost_usd,
        expected_cost
    );
    assert!((expected_cost - 0.002).abs() < 1e-10);
}

/// Empty assistant response (no text, no tools) produces Complete with empty content.
#[tokio::test]
async fn run_empty_assistant_response() {
    let provider = MockProvider::new(vec![ModelResponse {
        text: None,
        thinking: Vec::new(),
        tool_calls: Vec::new(),
        usage: UsageResponse {
            input_tokens: 5,
            output_tokens: 0,
            ..UsageResponse::default()
        },
    }]);
    let config = test_config();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();
    assert_eq!(result.status, ResultStatus::Complete);
    assert!(result.content.is_empty());
    // No assistant message pushed when content is empty
    assert_eq!(context.messages.len(), 1);
}

/// Mixed text and tool calls returns ToolCallsPending with both blocks.
#[tokio::test]
async fn run_mixed_text_and_tool_calls() {
    let provider = MockProvider::new(vec![mixed_response(
        "I'll read the file.",
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        0,
        0,
    )]);
    let config = test_config_with_tools();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let has_text = result
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text == "I'll read the file."));
    let has_tool = result
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(has_text, "should have text block");
    assert!(has_tool, "should have tool_use block");

    // Assistant message added to context
    assert_eq!(context.messages.len(), 2);
    let assistant = &context.messages[1];
    assert!(assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. })));
    assert!(assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. })));
}

/// Provider params are forwarded correctly from config.
#[tokio::test]
async fn run_forwards_correct_params_to_provider() {
    let config = Config::parse(
        r#"
system_prompt = "Test system prompt"

[model]
provider = "test"
name = "test-model-456"
max_tokens = 4096
temperature = 0.5

[provider.test]
api = "chat_completions"

[[tools]]
name = "read_file"
description = "Read a file"
parameters = { type = "object", properties = { path = { type = "string" } }, required = ["path"] }

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#,
    )
    .expect("test config should parse");

    let provider = MockProvider::new(vec![text_response("hello", 10, 5)]);
    let mut context = Context::default();
    context.push_user_text("test query").unwrap();

    runner::run(&config, &provider, &mut context)
        .await
        .unwrap();

    let captured = provider.captured_params();
    assert_eq!(captured.len(), 1);
    let p = &captured[0];
    assert_eq!(p.model, "test-model-456");
    assert_eq!(p.max_tokens, Some(4096));
    assert_eq!(p.temperature, Some(0.5));
    assert_eq!(p.system_prompt.as_deref(), Some("Test system prompt"));
    assert_eq!(p.messages.len(), 1);
    assert!(!p.tools.is_empty());
    assert_eq!(p.reasoning, None);
    assert!(p.output_schema.is_none());
}

/// Multiple tool calls in a single response all appear in result.
#[tokio::test]
async fn run_multiple_tool_calls() {
    let provider = MockProvider::new(vec![tool_call_response(
        vec![
            ("tc_a", "read_file", r#"{"path":"/a"}"#),
            ("tc_b", "read_file", r#"{"path":"/b"}"#),
        ],
        0,
        0,
    )]);
    let config = test_config_with_tools();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let tool_uses: Vec<_> = result
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .collect();
    assert_eq!(tool_uses.len(), 2);
}

/// Malformed tool call arguments return an error.
#[tokio::test]
async fn run_malformed_tool_arguments_error() {
    let provider = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", "not json at all")],
        10,
        5,
    )]);
    let config = test_config_with_tools();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let err = runner::run(&config, &provider, &mut context).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("malformed tool call arguments"), "unexpected error: {msg}");
    assert!(msg.contains("read_file"), "should mention tool name: {msg}");
}

/// Non-zero cache tokens flow through runner::run into the result usage.
#[tokio::test]
async fn run_cache_tokens_forwarded() {
    let provider = MockProvider::new(vec![text_response_with_cache(
        "cached", 1000, 500, 500, 300,
    )]);
    let config = test_config();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    let usage = result.usage.expect("usage should be present");
    assert_eq!(usage.cache_creation_input_tokens, 500);
    assert_eq!(usage.cache_read_input_tokens, 300);
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.output_tokens, 500);
}

/// Context overflow from push_assistant propagates as Err from runner::run.
#[tokio::test]
async fn run_context_overflow_propagates() {
    let provider = MockProvider::new(vec![text_response("done", 10, 5)]);
    let config = test_config();
    let mut context = Context::default();

    // Fill context to MAX_CONTEXT_MESSAGES (1024)
    for i in 0..1024 {
        if i % 2 == 0 {
            context.push_user_text(format!("msg {i}")).unwrap();
        } else {
            context.push_assistant(vec![ContentBlock::Text {
                text: format!("msg {i}"),
            }])
            .unwrap();
        }
    }
    assert_eq!(context.messages.len(), 1024);

    let result = runner::run(&config, &provider, &mut context).await;
    assert!(matches!(
        result,
        Err(FlickError::ContextOverflow(1024))
    ));
}

/// build_content preserves ordering: thinking, then text, then tool_use.
#[tokio::test]
async fn run_content_block_ordering() {
    let provider = MockProvider::new(vec![full_response(
        vec![("deep thought", "sig123")],
        Some("I'll read the file."),
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        10,
        5,
    )]);
    let config = test_config_with_tools();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &provider, &mut context).await.unwrap();

    assert_eq!(result.content.len(), 3);
    assert!(
        matches!(&result.content[0], ContentBlock::Thinking { text, .. } if text == "deep thought"),
        "index 0 should be Thinking, got {:?}",
        result.content[0]
    );
    assert!(
        matches!(&result.content[1], ContentBlock::Text { text } if text == "I'll read the file."),
        "index 1 should be Text, got {:?}",
        result.content[1]
    );
    assert!(
        matches!(&result.content[2], ContentBlock::ToolUse { name, .. } if name == "read_file"),
        "index 2 should be ToolUse, got {:?}",
        result.content[2]
    );
}
