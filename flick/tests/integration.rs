#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::pin::Pin;

use common::*;

use flick::agent;
use flick::context::Context;
use flick::error::ProviderError;
use flick::provider::{DynProvider, ModelResponse, RequestParams, ThinkingContent, UsageResponse};
use flick::tool::ToolRegistry;

// -- Integration tests --------------------------------------------------------

/// Full text-only conversation: config → context → agent loop → done event.
#[tokio::test]
async fn end_to_end_text_only() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[pricing]
input_per_million = 3.0
output_per_million = 15.0
"#,
    )
    .await;

    let provider = MockProvider::new(vec![text_response("Hello world", 50, 20)]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("Say hello").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // Verify event sequence: Usage, Text, Done
    assert!(emitter.events.iter().any(|e| matches!(e, flick::event::Event::Text { text } if text == "Hello world")));
    assert!(emitter.events.iter().any(|e| matches!(e, flick::event::Event::Usage { input_tokens: 50, output_tokens: 20, .. })));
    assert!(emitter.events.iter().any(|e| matches!(e, flick::event::Event::Done { .. })));

    // Verify done usage
    let done = emitter.events.iter().find_map(|e| {
        if let flick::event::Event::Done { usage } = e { Some(usage) } else { None }
    });
    if let Some(usage) = done {
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.iterations, 1);
        assert!(usage.cost_usd > 0.0);
    }

    // Verify context has user + assistant messages
    assert_eq!(context.messages.len(), 2);
    assert_eq!(context.messages[0].role, flick::context::Role::User);
    assert_eq!(context.messages[1].role, flick::context::Role::Assistant);
}

/// Tool call iteration: model calls tool, gets result, responds with text.
#[tokio::test]
async fn end_to_end_tool_call_then_text() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[tools]
read_file = true
"#,
    )
    .await;

    let step1 = tool_call_response(
        vec![("tc_1", "read_file", r#"{"path":"/nonexistent"}"#)],
        100, 30,
    );
    let step2 = text_response("The file was not found.", 200, 40);

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("read /nonexistent").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // Should have ToolResult event
    let tool_result_count = emitter
        .events
        .iter()
        .filter(|e| matches!(e, flick::event::Event::ToolResult { .. }))
        .count();
    assert_eq!(tool_result_count, 1);

    // Should end with Done with accumulated usage
    let done = emitter
        .events
        .iter()
        .find(|e| matches!(e, flick::event::Event::Done { .. }));
    if let Some(flick::event::Event::Done { usage }) = done {
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 70);
        assert_eq!(usage.iterations, 2);
    } else {
        panic!("expected Done event");
    }

    // Context: user, assistant(tool_use), user(tool_result), assistant(text)
    assert_eq!(context.messages.len(), 4);
}

/// Thinking blocks are accumulated and stored in context.
#[tokio::test]
async fn end_to_end_thinking_blocks() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    let provider = MockProvider::new(vec![ModelResponse {
        text: Some("Answer".into()),
        thinking: vec![ThinkingContent {
            text: "Let me reason".into(),
            signature: "sig_test_123".into(),
        }],
        tool_calls: Vec::new(),
        usage: UsageResponse::default(),
        warnings: Vec::new(),
    }]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("think about this").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    assert!(emitter.events.iter().any(|e| matches!(e, flick::event::Event::Thinking { .. })),
        "Thinking event should be emitted");
    assert!(emitter.events.iter().any(|e| matches!(e, flick::event::Event::ThinkingSignature { .. })),
        "ThinkingSignature event should be emitted");

    // Assistant message should have thinking block with signature
    let assistant = &context.messages[1];
    let has_thinking = assistant.content.iter().any(|b| {
        matches!(
            b,
            flick::context::ContentBlock::Thinking { text, signature }
                if text == "Let me reason" && signature == "sig_test_123"
        )
    });
    assert!(has_thinking, "expected thinking block in assistant message");
}

/// Malformed tool JSON triggers self-correction feedback.
#[tokio::test]
async fn end_to_end_malformed_tool_json_feedback() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[tools]
read_file = true
"#,
    )
    .await;

    let step1 = tool_call_response(
        vec![("tc_bad", "read_file", "not valid json{")],
        0, 0,
    );
    let step2 = text_response("Sorry", 0, 0);

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // ToolResult should report the parse error
    let tool_result = emitter.events.iter().find_map(|e| {
        if let flick::event::Event::ToolResult {
            success, output, ..
        } = e
        {
            Some((*success, output.clone()))
        } else {
            None
        }
    });
    assert!(tool_result.is_some());
    let (success, output) = tool_result.expect("tool result exists");
    assert!(!success);
    assert!(output.contains("invalid tool arguments JSON"));
}

/// JSON-lines emitter formats events correctly end-to-end.
#[tokio::test]
async fn end_to_end_json_lines_output() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    let provider = MockProvider::new(vec![text_response("hi", 0, 0)]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("hello").unwrap();

    let mut buf = Vec::new();
    {
        let mut emitter = flick::event::JsonLinesEmitter::new(&mut buf);
        let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
        result.expect("should succeed");
    }

    let output = String::from_utf8(buf).expect("valid utf8");
    let lines: Vec<&str> = output.trim().lines().collect();
    // usage + text + done = 3 lines
    assert!(lines.len() >= 2);

    // Each line should be valid JSON
    for line in &lines {
        let _: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    }

    // Should have text and done events
    let has_text = lines.iter().any(|l| l.contains("\"type\":\"text\""));
    let has_done = lines.iter().any(|l| l.contains("\"type\":\"done\""));
    assert!(has_text, "should have text event");
    assert!(has_done, "should have done event");
}

/// Raw emitter outputs only text content.
#[tokio::test]
async fn end_to_end_raw_output() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    let provider = MockProvider::new(vec![text_response("Hello world", 10, 5)]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("greet").unwrap();

    let mut buf = Vec::new();
    {
        let mut emitter = flick::event::RawEmitter::new(&mut buf);
        let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
        result.expect("should succeed");
    }

    let output = String::from_utf8(buf).expect("valid utf8");
    // Raw mode: only text + trailing newline from Done
    assert_eq!(output, "Hello world\n");
}

/// Context round-trip: save context, reload, continue conversation.
#[tokio::test]
async fn end_to_end_context_persistence() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    // First turn
    let provider1 = MockProvider::new(vec![text_response("First reply", 0, 0)]);
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("hello").unwrap();
    let mut emitter = CollectingEmitter::new();
    agent::run(&config, &provider1, &tools, &mut context, &mut emitter)
        .await
        .expect("first turn");

    // Serialize context
    let json = serde_json::to_string(&context).expect("serialize context");

    // Deserialize and continue
    let mut context2: Context = serde_json::from_str(&json).expect("deserialize context");
    assert_eq!(context2.messages.len(), 2);

    context2.push_user_text("follow up").unwrap();
    let provider2 = MockProvider::new(vec![text_response("Second reply", 0, 0)]);
    let mut emitter2 = CollectingEmitter::new();
    agent::run(&config, &provider2, &tools, &mut context2, &mut emitter2)
        .await
        .expect("second turn");

    assert_eq!(context2.messages.len(), 4);
}

/// Context loaded from disk file continues conversation.
#[tokio::test]
async fn end_to_end_context_file_loading() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    // Build a context with one turn of history
    let mut original = Context::default();
    original.push_user_text("first question").unwrap();
    original.push_assistant(vec![flick::context::ContentBlock::Text {
        text: "first answer".into(),
    }]).unwrap();

    // Write to temp file
    let json = serde_json::to_string(&original).expect("serialize context");
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    {
        use std::io::Write;
        f.write_all(json.as_bytes()).expect("write temp file");
    }

    // Load from disk
    let mut context = flick::context::Context::load_from_file(f.path())
        .await
        .expect("load context from file");
    assert_eq!(context.messages.len(), 2, "loaded context should have 2 messages");

    // Add a follow-up and run agent
    context.push_user_text("follow-up question").unwrap();

    let provider = MockProvider::new(vec![text_response("follow-up answer", 0, 0)]);
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    assert_eq!(context.messages.len(), 4);
    assert_eq!(context.messages[0].role, flick::context::Role::User);
    assert_eq!(context.messages[1].role, flick::context::Role::Assistant);
    assert_eq!(context.messages[2].role, flick::context::Role::User);
    assert_eq!(context.messages[3].role, flick::context::Role::Assistant);

    assert!(matches!(
        &context.messages[3].content[0],
        flick::context::ContentBlock::Text { text } if text == "follow-up answer"
    ));
}

/// Context with `ToolUse` + `ToolResult` history loads and continues correctly.
#[tokio::test]
async fn end_to_end_context_with_tool_history() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[tools]
read_file = true
"#,
    )
    .await;

    // Build context with tool use history
    let mut original = Context::default();
    original.push_user_text("read file").unwrap();
    original.push_assistant(vec![flick::context::ContentBlock::ToolUse {
        id: "tc_1".into(),
        name: "read_file".into(),
        input: serde_json::json!({"path": "/tmp/test"}),
    }]).unwrap();
    original.push_tool_results(vec![flick::context::ContentBlock::ToolResult {
        tool_use_id: "tc_1".into(),
        content: "file contents".into(),
        is_error: false,
    }]).unwrap();
    original.push_assistant(vec![flick::context::ContentBlock::Text {
        text: "I read the file.".into(),
    }]).unwrap();

    // Serialize and reload
    let json = serde_json::to_string(&original).expect("serialize");
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    {
        use std::io::Write;
        f.write_all(json.as_bytes()).expect("write");
    }
    let mut context = Context::load_from_file(f.path()).await.expect("load context");
    assert_eq!(context.messages.len(), 4);

    context.push_user_text("follow-up").unwrap();
    let provider = MockProvider::new(vec![text_response("follow-up answer", 0, 0)]);
    let tools = ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");
    assert_eq!(context.messages.len(), 6);
}

/// `shell_exec` through the full agent loop.
#[tokio::test]
async fn end_to_end_shell_exec() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[tools]
shell_exec = true
"#,
    )
    .await;

    let step1 = tool_call_response(
        vec![("tc_sh", "shell_exec", r#"{"command":"echo hello_from_shell"}"#)],
        40, 15,
    );
    let step2 = text_response("Done.", 80, 5);

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = flick::tool::ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("run echo").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    let tool_result = emitter.events.iter().find_map(|e| {
        if let flick::event::Event::ToolResult { success, output, .. } = e {
            Some((*success, output.clone()))
        } else {
            None
        }
    });
    assert!(tool_result.is_some());
    let (success, output) = tool_result.unwrap();
    assert!(success, "shell_exec should succeed");
    assert!(output.contains("hello_from_shell"), "output should contain echo result");

    // Context: user, assistant(tool_use), user(tool_result), assistant(text)
    assert_eq!(context.messages.len(), 4);
}

/// Custom tool execution through the full agent loop.
#[tokio::test]
async fn end_to_end_custom_tool() {
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"

[[tools.custom]]
name = "greet"
description = "Greets someone"
command = "echo hello {{name}}"
parameters = {type = "object", properties = {name = {type = "string"}}, required = ["name"]}
"#,
    )
    .await;

    let step1 = tool_call_response(
        vec![("tc_custom", "greet", r#"{"name":"world"}"#)],
        0, 0,
    );
    let step2 = text_response("Greeted.", 0, 0);

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = flick::tool::ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("greet world").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    let tool_result = emitter.events.iter().find_map(|e| {
        if let flick::event::Event::ToolResult { success, output, .. } = e {
            Some((*success, output.clone()))
        } else {
            None
        }
    });
    assert!(tool_result.is_some());
    let (success, output) = tool_result.unwrap();
    assert!(success, "custom tool should succeed");
    assert!(output.contains("hello world"), "output should contain greeting: got {output}");

    assert_eq!(context.messages.len(), 4);
}

struct ErrorProvider;
impl DynProvider for ErrorProvider {
    fn call_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>,
    > {
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
    let config = load_config(
        r#"
[model]
provider = "test"
name = "mock-model"

[provider.test]
api = "messages"
"#,
    )
    .await;

    let provider = ErrorProvider;
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    assert!(
        matches!(result, Err(flick::error::FlickError::Provider(ProviderError::AuthFailed))),
        "expected AuthFailed, got {result:?}"
    );
}
