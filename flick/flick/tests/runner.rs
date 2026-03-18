#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;

use std::pin::Pin;

use flick::ApiKind;
use flick::config::RequestConfig;
use flick::context::{ContentBlock, Context};
use flick::error::{FlickError, ProviderError};
use flick::model_registry::ModelInfo;
use flick::provider::{DynProvider, ModelResponse, RequestParams, UsageResponse};
use flick::result::ResultStatus;
use flick::runner;

fn test_config() -> RequestConfig {
    RequestConfig::parse_yaml("model: test\n").expect("test config should parse")
}

fn test_config_with_tools() -> RequestConfig {
    RequestConfig::parse_yaml(
        r"
model: test
tools:
  - name: read_file
    description: Read a file
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
",
    )
    .expect("test config should parse")
}

fn test_mi() -> ModelInfo {
    test_model_info_with_pricing(1.0, 2.0)
}

/// Single call returning Complete.
#[tokio::test]
async fn run_single_call_complete() {
    let provider = MockProvider::new(vec![text_response("done", 100, 50)]);
    let config = test_config();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.status, ResultStatus::Complete);
    assert_eq!(result.content.len(), 1);
    assert!(matches!(&result.content[0], ContentBlock::Text { text } if text == "done"));
    assert!(result.context_hash.is_none());

    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
}

/// Single call returning `ToolCallsPending`.
#[tokio::test]
async fn run_single_call_tool_calls_pending() {
    let provider = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        200,
        80,
    )]);
    let config = test_config_with_tools();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("read the file").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let tool_use_count = result
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .count();
    assert_eq!(tool_use_count, 1);
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
            Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>,
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
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::Messages,
        &ErrorProvider,
        &mut context,
    )
    .await;
    assert!(matches!(
        result,
        Err(FlickError::Provider(ProviderError::Api { status: 500, .. }))
    ));
}

/// `build_params` maps all config fields correctly.
#[test]
fn build_params_maps_config_fields() {
    let config = RequestConfig::parse_yaml(
        r#"
model: test
system_prompt: "Be helpful"
reasoning:
  level: high
output_schema:
  schema:
    type: object
"#,
    )
    .expect("test config should parse");

    let mi = ModelInfo {
        provider: "test".into(),
        name: "test-model-123".into(),
        max_tokens: Some(2048),
        input_per_million: None,
        output_per_million: None,
        cache_creation_per_million: None,
        cache_read_per_million: None,
    };

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

    let params = runner::build_params(&config, &mi, &messages, &tool_defs);

    assert_eq!(params.model, "test-model-123");
    assert_eq!(params.max_tokens, Some(2048));
    // Temperature stripped when reasoning is active
    assert_eq!(params.temperature, None);
    assert_eq!(params.system_prompt, Some("Be helpful"));
    assert_eq!(params.messages.len(), 1);
    assert_eq!(params.tools.len(), 1);
    assert_eq!(params.reasoning, Some(flick::model::ReasoningLevel::High));
    assert!(params.output_schema.is_some());
}

/// Cost is computed correctly in the usage summary.
#[tokio::test]
async fn run_cost_in_result() {
    let provider = MockProvider::new(vec![text_response("answer", 1000, 500)]);
    let config = test_config();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    let usage = result.usage.unwrap();
    let expected_cost = config.compute_cost(&mi, 1000, 500, 0, 0);
    assert!(
        (usage.cost_usd - expected_cost).abs() < 1e-10,
        "cost_usd ({}) should match compute_cost ({})",
        usage.cost_usd,
        expected_cost
    );
    assert!((expected_cost - 0.002).abs() < 1e-10);
}

/// Empty assistant response produces Complete with empty content.
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
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();
    assert_eq!(result.status, ResultStatus::Complete);
    assert!(result.content.is_empty());
    assert_eq!(context.messages.len(), 1);
}

/// Mixed text and tool calls returns `ToolCallsPending`.
#[tokio::test]
async fn run_mixed_text_and_tool_calls() {
    let provider = MockProvider::new(vec![mixed_response(
        "I'll read the file.",
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        0,
        0,
    )]);
    let config = test_config_with_tools();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let has_text = result
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    let has_tool = result
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(has_text);
    assert!(has_tool);
}

/// Provider params are forwarded correctly from config.
#[tokio::test]
async fn run_forwards_correct_params_to_provider() {
    let config = RequestConfig::parse_yaml(
        r#"
model: test
system_prompt: "Test system prompt"
temperature: 0.5
tools:
  - name: read_file
    description: Read a file
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
"#,
    )
    .expect("test config should parse");

    let mi = ModelInfo {
        provider: "test".into(),
        name: "test-model-456".into(),
        max_tokens: Some(4096),
        input_per_million: Some(1.0),
        output_per_million: Some(2.0),
        cache_creation_per_million: None,
        cache_read_per_million: None,
    };

    let provider = MockProvider::new(vec![text_response("hello", 10, 5)]);
    let mut context = Context::default();
    context.push_user_text("test query").unwrap();

    runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
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

/// Multiple tool calls in a single response.
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
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let tool_use_count = result
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .count();
    assert_eq!(tool_use_count, 2);
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
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let err = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("malformed tool call arguments"));
    assert!(msg.contains("read_file"));
}

/// Non-zero cache tokens flow through.
#[tokio::test]
async fn run_cache_tokens_forwarded() {
    let provider = MockProvider::new(vec![text_response_with_cache(
        "cached", 1000, 500, 500, 300,
    )]);
    let config = test_config();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    let usage = result.usage.expect("usage should be present");
    assert_eq!(usage.cache_creation_input_tokens, 500);
    assert_eq!(usage.cache_read_input_tokens, 300);
}

/// Context overflow propagates as Err.
#[tokio::test]
async fn run_context_overflow_propagates() {
    let provider = MockProvider::new(vec![text_response("done", 10, 5)]);
    let config = test_config();
    let mi = test_mi();
    let mut context = Context::default();

    for i in 0..1024 {
        if i % 2 == 0 {
            context.push_user_text(format!("msg {i}")).unwrap();
        } else {
            context
                .push_assistant(vec![ContentBlock::Text {
                    text: format!("msg {i}"),
                }])
                .unwrap();
        }
    }
    assert_eq!(context.messages.len(), 1024);

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context).await;
    assert!(matches!(result, Err(FlickError::ContextOverflow(1024))));
}

/// Content block ordering: thinking, then text, then `tool_use`.
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
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.content.len(), 3);
    assert!(matches!(&result.content[0], ContentBlock::Thinking { .. }));
    assert!(matches!(&result.content[1], ContentBlock::Text { .. }));
    assert!(matches!(&result.content[2], ContentBlock::ToolUse { .. }));
}

// -- Two-step structured output tests --

fn test_config_chat_completions_with_schema() -> RequestConfig {
    RequestConfig::parse_yaml(
        r"
model: test
tools:
  - name: read_file
    description: Read a file
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string
    required: [answer]
",
    )
    .expect("test config should parse")
}

/// Two-step: `chat_completions` + tools + schema triggers two provider calls.
#[tokio::test]
async fn run_two_step_structured_output() {
    let provider = MockProvider::new(vec![
        text_response("thinking aloud", 100, 50),
        text_response(r#"{"answer":"42"}"#, 80, 30),
    ]);
    let config = test_config_chat_completions_with_schema();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("what is the answer?").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
    .await
    .unwrap();

    assert_eq!(result.status, ResultStatus::Complete);
    assert_eq!(result.content.len(), 1);

    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens, 180);
    assert_eq!(usage.output_tokens, 80);

    let captured = provider.captured_params();
    assert_eq!(captured.len(), 2);
    assert!(!captured[0].tools.is_empty());
    assert!(captured[0].output_schema.is_none());
    assert!(captured[1].tools.is_empty());
    assert!(captured[1].output_schema.is_some());
}

/// Two-step: when the first call returns tool calls, skip the second call.
#[tokio::test]
async fn run_two_step_skipped_when_tool_calls_pending() {
    let provider = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        100,
        50,
    )]);
    let config = test_config_chat_completions_with_schema();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("read a file").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
    .await
    .unwrap();

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let captured = provider.captured_params();
    assert_eq!(captured.len(), 1);
}

/// Messages API with tools + schema does NOT trigger two-step.
#[tokio::test]
async fn run_no_two_step_for_messages_api() {
    let config = test_config_chat_completions_with_schema();
    let mi = test_mi();

    let provider = MockProvider::new(vec![text_response(r#"{"answer":"42"}"#, 100, 50)]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    assert_eq!(result.status, ResultStatus::Complete);
    let captured = provider.captured_params();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].output_schema.is_some());
}

/// Chat completions with schema but no tools does NOT trigger two-step.
#[tokio::test]
async fn run_no_two_step_without_tools() {
    let config = RequestConfig::parse_yaml(
        r"
model: test
output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string
    required: [answer]
",
    )
    .expect("test config should parse");
    let mi = test_mi();

    let provider = MockProvider::new(vec![text_response(r#"{"answer":"42"}"#, 100, 50)]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
    .await
    .unwrap();

    assert_eq!(result.status, ResultStatus::Complete);
    let captured = provider.captured_params();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].output_schema.is_some());
}

/// Two-step: usage cost is computed from summed tokens.
#[tokio::test]
async fn run_two_step_cost_summed() {
    let provider = MockProvider::new(vec![
        text_response("step1", 1000, 500),
        text_response(r#"{"answer":"done"}"#, 2000, 300),
    ]);
    let config = test_config_chat_completions_with_schema();
    let mi = test_mi();
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
    .await
    .unwrap();

    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens, 3000);
    assert_eq!(usage.output_tokens, 800);
    let expected_cost = config.compute_cost(&mi, 3000, 800, 0, 0);
    assert!((usage.cost_usd - expected_cost).abs() < 1e-10);
}

/// Two-step: cache tokens from both calls are summed and affect cost.
#[tokio::test]
async fn run_two_step_cache_cost_summed() {
    let provider = MockProvider::new(vec![
        text_response_with_cache("step1", 1000, 500, 200, 300),
        text_response_with_cache(r#"{"answer":"done"}"#, 2000, 300, 400, 600),
    ]);
    let config = test_config_chat_completions_with_schema();
    let mi = ModelInfo {
        provider: "test".into(),
        name: "mock-model".into(),
        max_tokens: Some(1024),
        input_per_million: Some(1.0),
        output_per_million: Some(2.0),
        cache_creation_per_million: Some(1.25),
        cache_read_per_million: Some(0.10),
    };
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(
        &config,
        &mi,
        ApiKind::ChatCompletions,
        &provider,
        &mut context,
    )
    .await
    .unwrap();

    let usage = result.usage.unwrap();
    assert_eq!(usage.input_tokens, 3000);
    assert_eq!(usage.output_tokens, 800);
    assert_eq!(usage.cache_creation_input_tokens, 600);
    assert_eq!(usage.cache_read_input_tokens, 900);
    let expected_cost = config.compute_cost(&mi, 3000, 800, 600, 900);
    assert!((usage.cost_usd - expected_cost).abs() < 1e-10);
    assert!(expected_cost > config.compute_cost(&mi, 3000, 800, 0, 0));
}

/// Cache tokens affect cost when model has cache pricing.
#[tokio::test]
async fn run_cache_tokens_affect_cost() {
    let provider = MockProvider::new(vec![text_response_with_cache(
        "cached answer",
        1000,
        500,
        2000,
        3000,
    )]);
    let config = test_config();
    let mi = ModelInfo {
        provider: "test".into(),
        name: "mock-model".into(),
        max_tokens: Some(1024),
        input_per_million: Some(1.0),
        output_per_million: Some(2.0),
        cache_creation_per_million: Some(1.25),
        cache_read_per_million: Some(0.10),
    };
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(&config, &mi, ApiKind::Messages, &provider, &mut context)
        .await
        .unwrap();

    let usage = result.usage.unwrap();
    let expected_cost = config.compute_cost(&mi, 1000, 500, 2000, 3000);
    assert!(
        (usage.cost_usd - expected_cost).abs() < 1e-10,
        "cost_usd ({}) should match compute_cost ({})",
        usage.cost_usd,
        expected_cost
    );
    // Verify cache pricing is non-zero contribution
    let cost_without_cache = config.compute_cost(&mi, 1000, 500, 0, 0);
    assert!(expected_cost > cost_without_cache);
}
