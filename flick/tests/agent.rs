#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;

use std::pin::Pin;

use std::io::Write;

use flick::agent;
use flick::config::Config;
use flick::context::{ContentBlock, Context};
use flick::error::{FlickError, ProviderError};
use flick::event::{RunSummary, StreamEvent};
use flick::model::ReasoningLevel;
use flick::provider::{DynProvider, EventStream, RequestParams};
use flick::tool::ToolRegistry;

fn test_config() -> Config {
    Config::parse(r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#).expect("test config should parse")
}

fn test_config_with_read_file() -> Config {
    Config::parse(r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"

[tools]
read_file = true

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#).expect("test config should parse")
}

#[tokio::test]
async fn run_usage_accumulation_across_iterations() {
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_1".into(),
            arguments: r#"{"path":"/nonexistent"}"#.into(),
        },
        StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];
    let step2 = vec![
        StreamEvent::TextDelta {
            text: "done".into(),
        },
        StreamEvent::Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    let done = emitter
        .events
        .iter()
        .find_map(|e| if let StreamEvent::Done { usage } = e { Some(usage) } else { None });
    assert!(done.is_some());
    let fallback = RunSummary::default();
    let usage = done.unwrap_or(&fallback);
    assert_eq!(usage.input_tokens, 300);
    assert_eq!(usage.output_tokens, 150);
    assert_eq!(usage.iterations, 2);
}

#[tokio::test]
async fn run_iteration_limit_exhaustion() {
    let steps: Vec<Vec<StreamEvent>> = (0..26).map(|i| {
        vec![
            StreamEvent::ToolCallStart {
                call_id: format!("tc_{i}"),
                tool_name: "read_file".into(),
            },
            StreamEvent::ToolCallEnd {
                call_id: format!("tc_{i}"),
                arguments: r#"{"path":"/nonexistent"}"#.into(),
            },
        ]
    }).collect();

    let provider = MockProvider::new(steps);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    assert!(matches!(result, Err(FlickError::IterationLimit(25))));

    // C6/T5: No Done event should be emitted on iteration limit
    assert!(
        !emitter.events.iter().any(|e| matches!(e, StreamEvent::Done { .. })),
        "Done should not be emitted when iteration limit is reached"
    );
}

#[tokio::test]
async fn run_tool_call_delta_unknown_id() {
    let provider = MockProvider::new(vec![vec![
        StreamEvent::ToolCallDelta {
            call_id: "unknown_id".into(),
            arguments_delta: "garbage".into(),
        },
        StreamEvent::TextDelta { text: "ok".into() },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();
}

#[tokio::test]
async fn run_multiple_tool_calls_single_iteration() {
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_a".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_a".into(),
            arguments: r#"{"path":"/nonexistent_a"}"#.into(),
        },
        StreamEvent::ToolCallStart {
            call_id: "tc_b".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_b".into(),
            arguments: r#"{"path":"/nonexistent_b"}"#.into(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta { text: "done".into() }];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    assert_eq!(
        emitter.events.iter().filter(|e| matches!(e, StreamEvent::ToolResult { .. })).count(),
        2
    );
}

#[tokio::test]
async fn run_tool_call_end_empty_args_fallback() {
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallDelta {
            call_id: "tc_1".into(),
            arguments_delta: r#"{"path""#.into(),
        },
        StreamEvent::ToolCallDelta {
            call_id: "tc_1".into(),
            arguments_delta: r#":"/nonexistent"}"#.into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_1".into(),
            arguments: String::new(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta {
        text: "done".into(),
    }];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    let tool_result = emitter
        .events
        .iter()
        .find(|e| matches!(e, StreamEvent::ToolResult { .. }));
    assert!(tool_result.is_some(), "tool should execute with accumulated args");
}

#[tokio::test]
async fn run_mixed_text_and_tool_calls() {
    let step1 = vec![
        StreamEvent::TextDelta {
            text: "I'll read the file.".into(),
        },
        StreamEvent::ToolCallStart {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_1".into(),
            arguments: r#"{"path":"/nonexistent"}"#.into(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta {
        text: "done".into(),
    }];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    let has_text = emitter
        .events
        .iter()
        .any(|e| matches!(e, StreamEvent::TextDelta { text } if text == "I'll read the file."));
    let has_tool_result = emitter
        .events
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolResult { .. }));
    assert!(has_text, "text delta should be emitted");
    assert!(has_tool_result, "tool result should be emitted");

    let assistant = &context.messages[1];
    let has_text_block = assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    let has_tool_block = assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(has_text_block, "assistant should have Text block");
    assert!(has_tool_block, "assistant should have ToolUse block");

    // T7: Verify block ordering — Text before ToolUse
    let text_idx = assistant.content.iter().position(|b| matches!(b, ContentBlock::Text { .. }));
    let tool_idx = assistant.content.iter().position(|b| matches!(b, ContentBlock::ToolUse { .. }));
    assert!(text_idx < tool_idx, "Text block should come before ToolUse block");
}

#[tokio::test]
async fn run_stream_fatal_error_event_propagates() {
    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta { text: "partial".into() },
        StreamEvent::Error {
            message: "overloaded".into(),
            code: "overloaded_error".into(),
            fatal: true,
        },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    assert!(matches!(result, Err(FlickError::Provider(ProviderError::StreamError(ref message))) if message.contains("overloaded")));
    assert_eq!(context.messages.len(), 1, "partial content must not be persisted on fatal error");
}

#[tokio::test]
async fn run_stream_nonfatal_error_does_not_abort() {
    // max_tokens truncation is non-fatal: partial text should be kept and
    // the agent loop should complete normally.
    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta { text: "partial answer".into() },
        StreamEvent::Error {
            message: "model response truncated (max tokens exceeded)".into(),
            code: "max_tokens".into(),
            fatal: false,
        },
        StreamEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    // The non-fatal error should be emitted to the consumer
    let has_error = emitter.events.iter().any(|e| matches!(e, StreamEvent::Error { code, .. } if code == "max_tokens"));
    assert!(has_error, "non-fatal error should still be emitted");

    // The partial text should be preserved in context
    let has_text = context.messages.iter().any(|m| {
        m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "partial answer"))
    });
    assert!(has_text, "partial text from truncated response should be in context");
}

/// Mock provider that yields a few Ok events then an error.
struct MidStreamErrorProvider;

impl DynProvider for MidStreamErrorProvider {
    fn stream_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<EventStream, ProviderError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "partial".into(),
                }),
                Err(ProviderError::Api {
                    status: 500,
                    message: "simulated mid-stream error".into(),
                }),
            ];
            let stream = tokio_stream::iter(events);
            Ok(Box::pin(stream) as EventStream)
        })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(serde_json::json!({}))
    }
}

#[tokio::test]
async fn run_mid_stream_provider_error() {
    let provider = MidStreamErrorProvider;
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    assert!(result.is_err());
    assert!(
        matches!(result, Err(FlickError::Provider(ProviderError::Api { status: 500, .. }))),
        "should propagate mid-stream API error"
    );
}

#[test]
fn build_params_maps_config_fields() {
    // Use chat_completions to avoid Anthropic budget_tokens validation
    // (high reasoning budget_tokens=32000 > max_tokens=2048 would fail for Anthropic)
    let config = Config::parse(r#"
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
"#).expect("test config should parse");

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

    let params = agent::build_params(&config, &messages, &tool_defs);

    assert_eq!(params.model, "test-model-123");
    assert_eq!(params.max_tokens, 2048);
    // Temperature stripped because reasoning is active
    assert_eq!(params.temperature, None);
    assert_eq!(params.system_prompt, Some("Be helpful"));
    assert_eq!(params.messages.len(), 1);
    assert_eq!(params.tools.len(), 1);
    assert_eq!(params.reasoning, Some(ReasoningLevel::High));
    assert!(params.output_schema.is_some());
    assert_eq!(params.output_schema.unwrap()["type"], "object");
}

#[tokio::test]
async fn run_cost_in_done_event() {
    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "answer".into(),
        },
        StreamEvent::Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    let done = emitter
        .events
        .iter()
        .find_map(|e| {
            if let StreamEvent::Done { usage } = e {
                Some(usage)
            } else {
                None
            }
        });
    assert!(done.is_some());
    let usage = done.unwrap();
    let expected_cost = config.compute_cost(1000, 500);
    assert!(
        (usage.cost_usd - expected_cost).abs() < 1e-10,
        "Done cost_usd ({}) should match compute_cost ({})",
        usage.cost_usd,
        expected_cost
    );
    assert!((expected_cost - 0.002).abs() < 1e-10);
}

#[tokio::test]
async fn run_tool_call_end_unknown_id() {
    let provider = MockProvider::new(vec![vec![
        StreamEvent::ToolCallEnd {
            call_id: "unknown_id".into(),
            arguments: r#"{"path":"/tmp"}"#.into(),
        },
        StreamEvent::TextDelta { text: "ok".into() },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();
    assert!(!emitter.events.iter().any(|e| matches!(e, StreamEvent::ToolResult { .. })));
}

#[tokio::test]
async fn run_unknown_tool_name_returns_error_result() {
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_1".into(),
            tool_name: "nonexistent_tool".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_1".into(),
            arguments: r#"{"foo":"bar"}"#.into(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta { text: "ok".into() }];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult { success, .. } = e { Some(*success) } else { None }
    });
    assert_eq!(tool_result, Some(false), "unknown tool should return success: false");
}

#[tokio::test]
async fn run_tool_call_empty_id() {
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: String::new(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: String::new(),
            arguments: r#"{"path":"/nonexistent"}"#.into(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta { text: "done".into() }];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    // Tool should still execute and produce a result
    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult { call_id, .. } = e { Some(call_id.as_str()) } else { None }
    });
    assert_eq!(tool_result, Some(""), "tool result should have empty call_id");

    // ToolUse block in context should have empty id
    let assistant = &context.messages[1];
    let has_empty_id_tool = assistant.content.iter().any(|b| {
        matches!(b, ContentBlock::ToolUse { id, .. } if id.is_empty())
    });
    assert!(has_empty_id_tool, "assistant should have ToolUse with empty id");
}

#[tokio::test]
async fn run_forwards_correct_params_to_provider() {
    let config = Config::parse(r#"
system_prompt = "Test system prompt"

[model]
provider = "test"
name = "test-model-456"
max_tokens = 4096
temperature = 0.5

[provider.test]
api = "chat_completions"

[tools]
read_file = true

[pricing]
input_per_million = 1.0
output_per_million = 2.0
"#).expect("test config should parse");

    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta { text: "hello".into() },
        StreamEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ]]);
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test query").unwrap();
    let mut emitter = CollectingEmitter::new();

    agent::run(&config, &provider, &tools, &mut context, &mut emitter)
        .await
        .unwrap();

    let captured = provider.captured_params();
    assert_eq!(captured.len(), 1);
    let p = &captured[0];
    assert_eq!(p.model, "test-model-456");
    assert_eq!(p.max_tokens, 4096);
    assert_eq!(p.temperature, Some(0.5));
    assert_eq!(p.system_prompt.as_deref(), Some("Test system prompt"));
    assert_eq!(p.messages.len(), 1);
    assert!(!p.tools.is_empty());
    assert_eq!(p.reasoning, None);
    assert!(p.output_schema.is_none());
}

#[tokio::test]
async fn run_empty_assistant_response() {
    let provider = MockProvider::new(vec![vec![
        StreamEvent::Usage { input_tokens: 5, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 },
    ]]);
    let config = test_config();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();
    // C5: Empty assistant content is no longer pushed to context
    assert_eq!(context.messages.len(), 1);
}

#[tokio::test]
async fn run_successful_tool_exec_feeds_back_into_loop() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    tmp.write_all(b"file content here").unwrap();
    let path = tmp.path().to_string_lossy().replace('\\', "/");
    let args = format!(r#"{{"path":"{}"}}"#, path);

    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_ok".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_ok".into(),
            arguments: args,
        },
        StreamEvent::Usage {
            input_tokens: 50,
            output_tokens: 10,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];
    let step2 = vec![
        StreamEvent::TextDelta {
            text: "Got it".into(),
        },
        StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];

    let provider = MockProvider::new(vec![step1, step2]);
    let config = test_config_with_read_file();
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("read the file").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.unwrap();

    // Tool result should report success with file content
    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult { success, output, .. } = e {
            Some((*success, output.clone()))
        } else {
            None
        }
    });
    assert!(tool_result.is_some());
    let (success, output) = tool_result.unwrap();
    assert!(success, "tool exec should succeed");
    assert!(output.contains("file content here"), "output should contain file content");

    // Context: user, assistant(tool_use), user(tool_result), assistant(text)
    assert_eq!(context.messages.len(), 4);
    // ToolResult in context should not be an error
    let tool_result_block = context.messages[2].content.iter().find(|b| {
        matches!(b, ContentBlock::ToolResult { is_error, .. } if !is_error)
    });
    assert!(tool_result_block.is_some(), "tool result should have is_error=false");
}
