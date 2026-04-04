#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use flick::config::RequestConfig;
use flick::context::Message;
use flick::error::ProviderError;
use flick::model::ReasoningLevel;
use flick::model_registry::ModelInfo;
use flick::provider::{DynProvider, ModelResponse, RequestParams, ToolDefinition, UsageResponse};

/// Owned mirror of `RequestParams` for test assertions.
#[derive(Debug, Clone)]
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

/// Mock provider that returns canned `ModelResponse` values per call.
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
        let guard = self.captured.lock().expect("captured mutex poisoned");
        guard.clone()
    }
}

impl DynProvider for MockProvider {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
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
    }
}

/// Helper to build a tool-call `ModelResponse`.
pub fn tool_call_response(
    calls: Vec<(&str, &str, &str)>,
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
    }
}

/// Helper to build a text-only `ModelResponse` with custom cache token values.
pub fn text_response_with_cache(
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
) -> ModelResponse {
    ModelResponse {
        text: Some(text.to_string()),
        thinking: Vec::new(),
        tool_calls: Vec::new(),
        usage: UsageResponse {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        },
    }
}

/// Helper to build a `ModelResponse` with thinking, text, and tool calls.
pub fn full_response(
    thinking: Vec<(&str, &str)>,
    text: Option<&str>,
    calls: Vec<(&str, &str, &str)>,
    input_tokens: u64,
    output_tokens: u64,
) -> ModelResponse {
    use flick::provider::{ThinkingContent, ToolCallResponse};
    ModelResponse {
        text: text.map(str::to_string),
        thinking: thinking
            .into_iter()
            .map(|(t, s)| ThinkingContent {
                text: t.to_string(),
                signature: s.to_string(),
            })
            .collect(),
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
    }
}

/// Helper: parse a `RequestConfig` from YAML.
pub fn parse_config(yaml: &str) -> RequestConfig {
    RequestConfig::parse_yaml(yaml).unwrap_or_else(|e| panic!("config should parse: {e}"))
}

/// Helper: create a test `ModelInfo`.
pub fn test_model_info() -> ModelInfo {
    ModelInfo {
        provider: "test".into(),
        name: "mock-model".into(),
        max_tokens: Some(1024),
        input_per_million: None,
        output_per_million: None,
        cache_creation_per_million: None,
        cache_read_per_million: None,
    }
}

/// Helper: create a test `ModelInfo` with pricing.
pub fn test_model_info_with_pricing(input: f64, output: f64) -> ModelInfo {
    ModelInfo {
        provider: "test".into(),
        name: "mock-model".into(),
        max_tokens: Some(1024),
        input_per_million: Some(input),
        output_per_million: Some(output),
        cache_creation_per_million: None,
        cache_read_per_million: None,
    }
}
