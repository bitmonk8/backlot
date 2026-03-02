#![allow(dead_code, clippy::expect_used)]

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use flick::config::Config;
use flick::context::Message;
use flick::error::ProviderError;
use flick::event::{EventEmitter, StreamEvent};
use flick::model::ReasoningLevel;
use flick::provider::{DynProvider, EventStream, RequestParams, ToolDefinition};

/// Owned mirror of `RequestParams` for test assertions.
#[derive(Debug)]
pub struct CapturedParams {
    pub model: String,
    pub max_tokens: u32,
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

/// Mock provider that returns canned events per iteration.
pub struct MockProvider {
    steps: Vec<Vec<StreamEvent>>,
    call_count: AtomicUsize,
    captured: Mutex<Vec<CapturedParams>>,
}

impl MockProvider {
    pub const fn new(steps: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            steps,
            call_count: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    /// Returns all captured `RequestParams` from `stream_boxed` calls.
    pub fn captured_params(&self) -> Vec<CapturedParams> {
        let mut guard = self.captured.lock().expect("captured mutex poisoned");
        std::mem::take(&mut *guard)
    }
}

impl DynProvider for MockProvider {
    fn stream_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>,
    > {
        self.captured
            .lock()
            .expect("captured mutex poisoned")
            .push(CapturedParams::from_request(&params));
        let idx = self.call_count.fetch_add(1, Ordering::Relaxed);
        let events = if idx < self.steps.len() {
            self.steps[idx].clone()
        } else {
            panic!(
                "MockProvider called more times than steps provided (call {idx}, only {} steps)",
                self.steps.len()
            );
        };
        Box::pin(async move {
            let stream = tokio_stream::iter(events.into_iter().map(Ok));
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

/// Collects emitted events for assertions.
pub struct CollectingEmitter {
    pub events: Vec<StreamEvent>,
}

impl CollectingEmitter {
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }
}

impl EventEmitter for CollectingEmitter {
    fn emit(&mut self, event: &StreamEvent) {
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
