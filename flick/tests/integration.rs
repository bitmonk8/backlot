#![allow(clippy::expect_used)]

mod common;

use std::pin::Pin;

use common::*;

use flick::agent;
use flick::context::Context;
use flick::error::ProviderError;
use flick::event::StreamEvent;
use flick::provider::{DynProvider, EventStream, RequestParams};
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

    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "Hello".into(),
        },
        StreamEvent::TextDelta {
            text: " world".into(),
        },
        StreamEvent::Usage {
            input_tokens: 50,
            output_tokens: 20,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ]]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("Say hello").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // Verify event sequence: TextDelta, TextDelta, Usage, Done
    assert_eq!(emitter.events.len(), 4);
    assert!(matches!(&emitter.events[0], StreamEvent::TextDelta { text } if text == "Hello"));
    assert!(matches!(&emitter.events[1], StreamEvent::TextDelta { text } if text == " world"));
    assert!(matches!(&emitter.events[2], StreamEvent::Usage { input_tokens: 50, output_tokens: 20, .. }));
    assert!(matches!(&emitter.events[3], StreamEvent::Done { .. }));

    // Verify done usage
    if let StreamEvent::Done { usage } = &emitter.events[3] {
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

    // Iteration 1: model requests read_file
    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallDelta {
            call_id: "tc_1".into(),
            arguments_delta: r#"{"path":"/nonexistent"}"#.into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_1".into(),
            arguments: r#"{"path":"/nonexistent"}"#.into(),
        },
        StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 30,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];
    // Iteration 2: text response
    let step2 = vec![
        StreamEvent::TextDelta {
            text: "The file was not found.".into(),
        },
        StreamEvent::Usage {
            input_tokens: 200,
            output_tokens: 40,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];

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
        .filter(|e| matches!(e, StreamEvent::ToolResult { .. }))
        .count();
    assert_eq!(tool_result_count, 1);

    // Should end with Done with accumulated usage
    let done = emitter
        .events
        .iter()
        .find(|e| matches!(e, StreamEvent::Done { .. }));
    if let Some(StreamEvent::Done { usage }) = done {
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

    let provider = MockProvider::new(vec![vec![
        StreamEvent::ThinkingDelta {
            text: "Let me ".into(),
        },
        StreamEvent::ThinkingDelta {
            text: "reason".into(),
        },
        StreamEvent::ThinkingSignature {
            signature: "sig_test_123".into(),
        },
        StreamEvent::TextDelta {
            text: "Answer".into(),
        },
    ]]);

    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut context = Context::default();
    context.push_user_text("think about this").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // T9: Assert ThinkingDelta and ThinkingSignature events were emitted
    assert!(emitter.events.iter().any(|e| matches!(e, StreamEvent::ThinkingDelta { .. })),
        "ThinkingDelta event should be emitted");
    assert!(emitter.events.iter().any(|e| matches!(e, StreamEvent::ThinkingSignature { .. })),
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

    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_bad".into(),
            tool_name: "read_file".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_bad".into(),
            arguments: "not valid json{".into(),
        },
    ];
    let step2 = vec![StreamEvent::TextDelta {
        text: "Sorry".into(),
    }];

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("test").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // ToolResult should report the parse error
    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult {
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

    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "hi".into(),
        },
    ]]);

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
    assert_eq!(lines.len(), 2); // text_delta + done

    // Each line should be valid JSON with expected type values
    let p0: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
    assert_eq!(p0["type"], "text_delta");
    let p1: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
    assert_eq!(p1["type"], "done");
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

    let provider = MockProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "Hello".into(),
        },
        StreamEvent::TextDelta {
            text: " world".into(),
        },
        StreamEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ]]);

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
    // Raw mode: only text deltas + trailing newline from Done
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
    let provider1 = MockProvider::new(vec![vec![StreamEvent::TextDelta {
        text: "First reply".into(),
    }]]);
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
    let provider2 = MockProvider::new(vec![vec![StreamEvent::TextDelta {
        text: "Second reply".into(),
    }]]);
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

    let provider = MockProvider::new(vec![vec![StreamEvent::TextDelta {
        text: "follow-up answer".into(),
    }]]);
    let tools = ToolRegistry::from_config(config.tools(), vec![]);
    let mut emitter = CollectingEmitter::new();

    let result = agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    // Context should now have 4 messages: user, assistant, user, assistant
    assert_eq!(context.messages.len(), 4);
    assert_eq!(context.messages[0].role, flick::context::Role::User);
    assert_eq!(context.messages[1].role, flick::context::Role::Assistant);
    assert_eq!(context.messages[2].role, flick::context::Role::User);
    assert_eq!(context.messages[3].role, flick::context::Role::Assistant);

    // Verify follow-up assistant content text
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
    let provider = MockProvider::new(vec![vec![StreamEvent::TextDelta {
        text: "follow-up answer".into(),
    }]]);
    let tools = ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");
    assert_eq!(context.messages.len(), 6);
}

/// FL46: shell_exec through the full agent loop.
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

    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_sh".into(),
            tool_name: "shell_exec".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_sh".into(),
            arguments: r#"{"command":"echo hello_from_shell"}"#.into(),
        },
        StreamEvent::Usage {
            input_tokens: 40,
            output_tokens: 15,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];
    let step2 = vec![
        StreamEvent::TextDelta {
            text: "Done.".into(),
        },
        StreamEvent::Usage {
            input_tokens: 80,
            output_tokens: 5,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    ];

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = flick::tool::ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("run echo").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult { success, output, .. } = e {
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

/// FL47: Custom tool execution through the full agent loop.
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

    let step1 = vec![
        StreamEvent::ToolCallStart {
            call_id: "tc_custom".into(),
            tool_name: "greet".into(),
        },
        StreamEvent::ToolCallEnd {
            call_id: "tc_custom".into(),
            arguments: r#"{"name":"world"}"#.into(),
        },
    ];
    let step2 = vec![
        StreamEvent::TextDelta {
            text: "Greeted.".into(),
        },
    ];

    let provider = MockProvider::new(vec![step1, step2]);
    let tools = flick::tool::ToolRegistry::from_config(config.tools(), config.resources().to_vec());
    let mut context = Context::default();
    context.push_user_text("greet world").unwrap();
    let mut emitter = CollectingEmitter::new();

    let result = flick::agent::run(&config, &provider, &tools, &mut context, &mut emitter).await;
    result.expect("should succeed");

    let tool_result = emitter.events.iter().find_map(|e| {
        if let StreamEvent::ToolResult { success, output, .. } = e {
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
    fn stream_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
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

/// Provider returning an error mid-stream propagates to caller.
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
