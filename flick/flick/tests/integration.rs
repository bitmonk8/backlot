#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::pin::Pin;

use common::*;

use flick::ApiKind;
use flick::context::{ContentBlock, Context};
use flick::error::{FlickError, ProviderError};
use flick::provider::{DynProvider, ModelResponse, RequestParams, ThinkingContent, UsageResponse};
use flick::result::{FlickResult, ResultError, ResultStatus, UsageSummary};
use flick::runner;
use xxhash_rust::xxh3::xxh3_128;

// -- Integration tests --------------------------------------------------------

/// Full text-only conversation: config -> context -> single model call -> Complete result.
#[tokio::test]
async fn end_to_end_text_only() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info_with_pricing(3.0, 15.0);

    let provider = MockProvider::new(vec![text_response("Hello world", 50, 20)]);

    let mut context = Context::default();
    context.push_user_text("Say hello").unwrap();

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    assert_eq!(result.status, ResultStatus::Complete);
    assert!(
        result
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "Hello world"))
    );

    let usage = result.usage.expect("usage should be present");
    assert_eq!(usage.input_tokens, 50);
    assert_eq!(usage.output_tokens, 20);
    assert!(usage.cost_usd > 0.0);

    assert_eq!(context.messages.len(), 2);
    assert_eq!(context.messages[0].role, flick::context::Role::User);
    assert_eq!(context.messages[1].role, flick::context::Role::Assistant);
}

/// Model returns tool calls: result is `ToolCallsPending`, caller handles execution.
#[tokio::test]
async fn end_to_end_tool_calls_pending() {
    let config = parse_config(
        r#"
model: test
tools:
  - name: read_file
    description: "Read a file's contents"
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
"#,
    );
    let model_info = test_model_info();

    let provider = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/nonexistent"}"#)],
        100,
        30,
    )]);

    let mut context = Context::default();
    context.push_user_text("read /nonexistent").unwrap();

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    assert_eq!(result.status, ResultStatus::ToolCallsPending);
    let tool_use_count = result
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
        .count();
    assert_eq!(tool_use_count, 1);
    assert_eq!(context.messages.len(), 2);
}

/// Thinking blocks are stored in result content and context.
#[tokio::test]
async fn end_to_end_thinking_blocks() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info();

    let provider = MockProvider::new(vec![ModelResponse {
        text: Some("Answer".into()),
        thinking: vec![ThinkingContent {
            text: "Let me reason".into(),
            signature: "sig_test_123".into(),
        }],
        tool_calls: Vec::new(),
        usage: UsageResponse::default(),
    }]);

    let mut context = Context::default();
    context.push_user_text("think about this").unwrap();

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    assert_eq!(result.status, ResultStatus::Complete);

    let has_thinking = result.content.iter().any(|b| {
        matches!(
            b,
            ContentBlock::Thinking { text, signature }
                if text == "Let me reason" && signature == "sig_test_123"
        )
    });
    assert!(has_thinking, "result should contain thinking block");

    let has_text = result
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { text } if text == "Answer"));
    assert!(has_text, "result should contain text block");

    let assistant = &context.messages[1];
    let has_thinking_in_ctx = assistant.content.iter().any(|b| {
        matches!(
            b,
            ContentBlock::Thinking { text, signature }
                if text == "Let me reason" && signature == "sig_test_123"
        )
    });
    assert!(has_thinking_in_ctx);
}

/// Context round-trip: save context, reload, continue conversation.
#[tokio::test]
async fn end_to_end_context_persistence() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info();

    let provider1 = MockProvider::new(vec![text_response("First reply", 0, 0)]);
    let mut context = Context::default();
    context.push_user_text("hello").unwrap();
    runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider1,
        &mut context,
    )
    .await
    .expect("first turn");

    let json = serde_json::to_string(&context).expect("serialize context");
    let mut context2: Context = serde_json::from_str(&json).expect("deserialize context");
    assert_eq!(context2.messages.len(), 2);

    context2.push_user_text("follow up").unwrap();
    let provider2 = MockProvider::new(vec![text_response("Second reply", 0, 0)]);
    runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider2,
        &mut context2,
    )
    .await
    .expect("second turn");

    assert_eq!(context2.messages.len(), 4);
}

/// Context loaded from disk file continues conversation.
#[tokio::test]
async fn end_to_end_context_file_loading() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info();

    let mut original = Context::default();
    original.push_user_text("first question").unwrap();
    original
        .push_assistant(vec![ContentBlock::Text {
            text: "first answer".into(),
        }])
        .unwrap();

    let json = serde_json::to_string(&original).expect("serialize context");
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    {
        use std::io::Write;
        f.write_all(json.as_bytes()).expect("write temp file");
    }

    let mut context = flick::context::Context::load_from_file(f.path())
        .await
        .expect("load context from file");
    assert_eq!(context.messages.len(), 2);

    context.push_user_text("follow-up question").unwrap();
    let provider = MockProvider::new(vec![text_response("follow-up answer", 0, 0)]);

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    assert_eq!(result.status, ResultStatus::Complete);
    assert_eq!(context.messages.len(), 4);
}

/// Context with `ToolUse` + `ToolResult` history loads and continues correctly.
#[tokio::test]
async fn end_to_end_context_with_tool_history() {
    let config = parse_config(
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
    );
    let model_info = test_model_info();

    let mut original = Context::default();
    original.push_user_text("read file").unwrap();
    original
        .push_assistant(vec![ContentBlock::ToolUse {
            id: "tc_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/test"}),
        }])
        .unwrap();
    original
        .push_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "tc_1".into(),
            content: "file contents".into(),
            is_error: false,
        }])
        .unwrap();
    original
        .push_assistant(vec![ContentBlock::Text {
            text: "I read the file.".into(),
        }])
        .unwrap();

    let json = serde_json::to_string(&original).expect("serialize");
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    {
        use std::io::Write;
        f.write_all(json.as_bytes()).expect("write");
    }
    let mut context = Context::load_from_file(f.path())
        .await
        .expect("load context");
    assert_eq!(context.messages.len(), 4);

    context.push_user_text("follow-up").unwrap();
    let provider = MockProvider::new(vec![text_response("follow-up answer", 0, 0)]);

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    assert_eq!(result.status, ResultStatus::Complete);
    assert_eq!(context.messages.len(), 6);
}

struct ErrorProvider;
impl DynProvider for ErrorProvider {
    fn call_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        Box::pin(async { Err(ProviderError::AuthFailed) })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(serde_json::json!({}))
    }
}

/// Provider returning an error propagates to caller.
#[tokio::test]
async fn end_to_end_provider_error_propagates() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info();

    let provider = ErrorProvider;
    let mut context = Context::default();
    context.push_user_text("test").unwrap();

    let result = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await;
    assert!(
        matches!(
            result,
            Err(flick::error::FlickError::Provider(
                ProviderError::AuthFailed
            ))
        ),
        "expected AuthFailed, got {result:?}"
    );
}

/// Simulated resume flow.
#[tokio::test]
async fn end_to_end_resume_flow() {
    let config = parse_config(
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
    );
    let model_info = test_model_info();

    let provider1 = MockProvider::new(vec![tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/tmp/test"}"#)],
        100,
        30,
    )]);
    let mut context = Context::default();
    context.push_user_text("read the file").unwrap();

    let result1 = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider1,
        &mut context,
    )
    .await
    .expect("first call");
    assert_eq!(result1.status, ResultStatus::ToolCallsPending);

    context
        .push_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "tc_1".into(),
            content: "file contents here".into(),
            is_error: false,
        }])
        .unwrap();

    let provider2 = MockProvider::new(vec![text_response("The file contains...", 200, 40)]);
    let result2 = runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider2,
        &mut context,
    )
    .await
    .expect("second call");
    assert_eq!(result2.status, ResultStatus::Complete);
    assert_eq!(context.messages.len(), 4);
}

#[test]
fn error_result_json_output_format() {
    let error = FlickError::NoQuery;
    let error_result = FlickResult {
        status: ResultStatus::Error,
        content: vec![],
        usage: None,
        context_hash: None,
        error: Some(ResultError {
            message: error.to_string(),
            code: error.code().to_string(),
        }),
    };

    let json_str = serde_json::to_string(&error_result).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("parse JSON");

    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "no_query");
    assert!(parsed.get("content").is_none());
}

#[test]
fn complete_result_json_output_format() {
    let result = FlickResult {
        status: ResultStatus::Complete,
        content: vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        usage: Some(UsageSummary {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.001,
        }),
        context_hash: Some("abcdef01234567890abcdef012345678".into()),
        error: None,
    };

    let json_str = serde_json::to_string(&result).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("parse JSON");

    assert_eq!(parsed["status"], "complete");
    assert_eq!(parsed["content"][0]["type"], "text");
    assert_eq!(parsed["content"][0]["text"], "answer");
    assert_eq!(parsed["usage"]["input_tokens"], 100);
}

#[tokio::test]
async fn context_hash_deterministic() {
    let config = parse_config("model: test\n");
    let model_info = test_model_info_with_pricing(3.0, 15.0);

    let provider = MockProvider::new(vec![text_response("Hash test", 10, 5)]);
    let mut context = Context::default();
    context.push_user_text("compute hash").unwrap();

    runner::run(
        &config,
        &model_info,
        ApiKind::Messages,
        &provider,
        &mut context,
    )
    .await
    .expect("should succeed");

    let context_bytes = serde_json::to_vec(&context).expect("serialize context");
    let hash = xxh3_128(&context_bytes);
    let hash_hex = format!("{hash:032x}");

    assert_eq!(hash_hex.len(), 32);
    assert!(
        hash_hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
    );

    let context_bytes_2 = serde_json::to_vec(&context).expect("serialize again");
    let hash_2 = xxh3_128(&context_bytes_2);
    let hash_hex_2 = format!("{hash_2:032x}");
    assert_eq!(hash_hex, hash_hex_2);
}
