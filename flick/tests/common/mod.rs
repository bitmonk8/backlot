#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use flick::config::Config;
use flick::context::Message;
use flick::error::ProviderError;
use flick::event::{EventEmitter, Event};
use flick::model::ReasoningLevel;
use flick::provider::{
    DynProvider, ModelResponse, RequestParams, ToolDefinition, UsageResponse,
};

/// Owned mirror of `RequestParams` for test assertions.
#[derive(Debug)]
pub struct CapturedParams {
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub reasoning: Option<ReasoningLevel>,
    pub output_schema: Option<serde_json::Value>,
}

impl CapturedParams {
    fn from_request(params: &RequestParams<'_>) -> Self {
        Self {
            model: params.model.to_string(),
            max_tokens: params.max_tokens,
            temperature: params.temperature,
            system_prompt: params.system_prompt.map(str::to_string),
            messages: params.messages.to_vec(),
            tools: params.tools.to_vec(),
            reasoning: params.reasoning,
            output_schema: params.output_schema.cloned(),
        }
    }
}

/// Mock provider that returns canned `ModelResponse` values per iteration.
pub struct MockProvider {
    steps: Mutex<Vec<Option<ModelResponse>>>,
    call_count: AtomicUsize,
    captured: Mutex<Vec<CapturedParams>>,
}

impl MockProvider {
    pub fn new(steps: Vec<ModelResponse>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().map(Some).collect()),
            call_count: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    /// Returns all captured `RequestParams` from `call_boxed` calls.
    pub fn captured_params(&self) -> Vec<CapturedParams> {
        let mut guard = self.captured.lock().expect("captured mutex poisoned");
        std::mem::take(&mut *guard)
    }
}

impl DynProvider for MockProvider {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>,
    > {
        self.captured
            .lock()
            .expect("captured mutex poisoned")
            .push(CapturedParams::from_request(&params));
        let idx = self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = {
            let mut steps = self.steps.lock().expect("steps mutex poisoned");
            assert!(
                idx < steps.len(),
                "MockProvider called more times than steps provided (call {idx}, only {} steps)",
                steps.len()
            );
            steps[idx]
                .take()
                .expect("MockProvider step already consumed")
        };
        Box::pin(async move { Ok(response) })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(serde_json::json!({}))
    }
}

/// Helper to build a text-only `ModelResponse`.
pub fn text_response(text: &str, input_tokens: u64, output_tokens: u64) -> ModelResponse {
    ModelResponse {
        text: Some(text.to_string()),
        thinking: Vec::new(),
        tool_calls: Vec::new(),
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        warnings: Vec::new(),
    }
}

/// Helper to build a tool-call `ModelResponse`.
pub fn tool_call_response(
    calls: Vec<(&str, &str, &str)>, // (call_id, tool_name, arguments)
    input_tokens: u64,
    output_tokens: u64,
) -> ModelResponse {
    use flick::provider::ToolCallResponse;
    ModelResponse {
        text: None,
        thinking: Vec::new(),
        tool_calls: calls
            .into_iter()
            .map(|(id, name, args)| ToolCallResponse {
                call_id: id.to_string(),
                tool_name: name.to_string(),
                arguments: args.to_string(),
            })
            .collect(),
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        warnings: Vec::new(),
    }
}

/// Helper to build a `ModelResponse` with both text and tool calls.
pub fn mixed_response(
    text: &str,
    calls: Vec<(&str, &str, &str)>,
    input_tokens: u64,
    output_tokens: u64,
) -> ModelResponse {
    use flick::provider::ToolCallResponse;
    ModelResponse {
        text: Some(text.to_string()),
        thinking: Vec::new(),
        tool_calls: calls
            .into_iter()
            .map(|(id, name, args)| ToolCallResponse {
                call_id: id.to_string(),
                tool_name: name.to_string(),
                arguments: args.to_string(),
            })
            .collect(),
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        warnings: Vec::new(),
    }
}

/// Collects emitted events for assertions.
pub struct CollectingEmitter {
    pub events: Vec<Event>,
}

impl CollectingEmitter {
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl EventEmitter for CollectingEmitter {
    fn emit(&mut self, event: &Event) {
        self.events.push(event.clone());
    }
}

pub fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().expect("create temp file");
    f.write_all(content.as_bytes()).expect("write temp file");
    f
}

pub async fn load_config(toml: &str) -> Config {
    let f = write_temp_config(toml);
    Config::load(f.path()).await.expect("config should parse")
}
