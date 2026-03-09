//! Reusable test doubles for `DynProvider`. Gated behind `#[cfg(test)]`.

use std::pin::Pin;
use std::sync::Mutex;

use crate::error::ProviderError;
use crate::provider::{DynProvider, ModelResponse, RequestParams, ToolCallResponse, UsageResponse};

/// Single-shot provider: returns one canned `ModelResponse`, then panics if
/// called again.
pub struct SingleShotProvider {
    response: Mutex<Option<ModelResponse>>,
    build_result: serde_json::Value,
}

impl SingleShotProvider {
    /// Minimal default: returns `"mock response"` text.
    pub fn stub() -> Box<Self> {
        Box::new(Self {
            response: Mutex::new(Some(ModelResponse {
                text: Some("mock response".into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            })),
            build_result: serde_json::json!({"model": "test-model", "messages": []}),
        })
    }

    /// Returns a text-only response.
    pub fn with_text(text: &str) -> Box<Self> {
        Box::new(Self {
            response: Mutex::new(Some(ModelResponse {
                text: Some(text.into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            })),
            build_result: serde_json::json!({"model": "test-model"}),
        })
    }

    /// Returns a tool-calls-only response.
    pub fn with_tool_calls(calls: Vec<ToolCallResponse>) -> Box<Self> {
        Box::new(Self {
            response: Mutex::new(Some(ModelResponse {
                text: None,
                thinking: Vec::new(),
                tool_calls: calls,
                usage: UsageResponse::default(),
            })),
            build_result: serde_json::json!({"model": "test-model"}),
        })
    }
}

impl DynProvider for SingleShotProvider {
    fn call_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        #[allow(clippy::expect_used)]
        let response = self
            .response
            .lock()
            .expect("mutex poisoned")
            .take()
            .expect("SingleShotProvider called more than once");
        Box::pin(async move { Ok(response) })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(self.build_result.clone())
    }
}

/// Multi-shot provider: returns canned responses in call order, errors when exhausted.
pub struct MultiShotProvider {
    responses: Mutex<Vec<ModelResponse>>,
}

impl MultiShotProvider {
    /// Takes responses in call-order; first element is returned on the first call.
    pub fn new(mut responses: Vec<ModelResponse>) -> Box<Self> {
        responses.reverse();
        Box::new(Self {
            responses: Mutex::new(responses),
        })
    }
}

impl DynProvider for MultiShotProvider {
    fn call_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        #[allow(clippy::expect_used)]
        let result = self
            .responses
            .lock()
            .expect("mutex poisoned")
            .pop()
            .ok_or_else(|| ProviderError::ResponseParse("MultiShotProvider exhausted".into()));
        Box::pin(async move { result })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(serde_json::json!({"model": "test"}))
    }
}

/// Build-only provider: implements `build_request` with a stub value and panics
/// on `call_boxed`. Useful for dry-run / request-building tests.
pub struct StubBuildProvider;

impl DynProvider for StubBuildProvider {
    fn call_boxed<'a>(
        &'a self,
        _params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        Box::pin(async { unreachable!("StubBuildProvider::call_boxed must not be called") })
    }

    fn build_request(
        &self,
        _params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Ok(serde_json::json!({"model": "test"}))
    }
}
