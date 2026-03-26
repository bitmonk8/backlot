pub mod config;
pub mod context;
mod crypto;
pub mod error;
pub mod history;
pub mod model;
pub mod model_list;
pub mod model_registry;
mod platform;
pub mod provider;
pub mod provider_registry;
pub mod result;
pub mod runner;
pub mod validation;

#[cfg(any(test, feature = "testing"))]
pub mod test_support;

use std::time::Duration;

use serde::{Deserialize, Serialize};

// Re-exports for convenience
pub use config::{ConfigFormat, RequestConfig, ToolConfig};
pub use context::{ContentBlock, Context, Message};
pub use error::FlickError;
pub use model_registry::{ModelInfo, ModelRegistry};
pub use provider::DynProvider;
use provider::create_provider;
pub use provider_registry::ProviderRegistry;
pub use result::{FlickResult, Timing};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    Messages,
    ChatCompletions,
}

impl std::fmt::Display for ApiKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Messages => f.write_str("messages"),
            Self::ChatCompletions => f.write_str("chat_completions"),
        }
    }
}

/// Reusable handle holding resolved config, model info, and provider.
///
/// Constructed via `FlickClient::new()` which resolves the full
/// `RequestConfig.model → ModelRegistry → ProviderRegistry` chain.
pub struct FlickClient {
    config: RequestConfig,
    model_info: ModelInfo,
    api_kind: ApiKind,
    provider: Box<dyn DynProvider>,
}

impl FlickClient {
    /// Build from a `RequestConfig` by resolving model and provider from registries.
    ///
    /// Resolution errors (unknown model, unknown provider) fail here, not at call time.
    pub async fn new(
        request: RequestConfig,
        models: &ModelRegistry,
        providers: &ProviderRegistry,
    ) -> Result<Self, FlickError> {
        let model_key = request.model();
        let model_info = models
            .get(model_key)
            .ok_or_else(|| {
                FlickError::Config(error::ConfigError::InvalidModelConfig(format!(
                    "unknown model key: '{model_key}'"
                )))
            })?
            .clone();

        // Resolve provider — returns error if not found.
        let provider_info = providers
            .get(&model_info.provider)
            .await
            .map_err(FlickError::Credential)?;

        // Validate request against resolved model/provider
        validation::validate_resolved_from_provider_info(&request, &model_info, &provider_info)?;

        let api_kind = provider_info.api;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| FlickError::Io(std::io::Error::other(e.to_string())))?;
        let provider = create_provider(&provider_info, client);

        Ok(Self {
            config: request,
            model_info,
            api_kind,
            provider: Box::new(provider),
        })
    }

    /// Build from a `RequestConfig`, model info, and an injected provider.
    /// For testing — skips registry resolution.
    #[cfg(any(test, feature = "testing"))]
    pub fn new_with_provider(
        config: RequestConfig,
        model_info: ModelInfo,
        api_kind: ApiKind,
        provider: Box<dyn DynProvider>,
    ) -> Self {
        Self {
            config,
            model_info,
            api_kind,
            provider,
        }
    }

    /// Single-shot query. Pushes the query as a user message, makes one model
    /// call, and returns the result. The caller owns and passes the `Context`.
    pub async fn run(&self, query: &str, context: &mut Context) -> Result<FlickResult, FlickError> {
        context.push_user_text(query)?;
        runner::run(
            &self.config,
            &self.model_info,
            self.api_kind,
            self.provider.as_ref(),
            context,
        )
        .await
    }

    /// Resume a session with tool results. Pushes the tool results, makes one
    /// model call, and returns the result.
    pub async fn resume(
        &self,
        context: &mut Context,
        tool_results: Vec<ContentBlock>,
    ) -> Result<FlickResult, FlickError> {
        context.push_tool_results(tool_results)?;
        runner::run(
            &self.config,
            &self.model_info,
            self.api_kind,
            self.provider.as_ref(),
            context,
        )
        .await
    }

    /// Build the API request body without sending it (dry-run).
    pub fn build_request(&self, query: &str) -> Result<serde_json::Value, FlickError> {
        let mut context = Context::default();
        context.push_user_text(query)?;
        let tool_defs: Vec<provider::ToolDefinition> = self
            .config
            .tools()
            .iter()
            .map(config::ToolConfig::to_definition)
            .collect();
        let params = runner::build_params(
            &self.config,
            &self.model_info,
            &context.messages,
            &tool_defs,
        );
        self.provider
            .build_request(params)
            .map_err(FlickError::Provider)
    }

    /// Access the underlying config.
    pub const fn config(&self) -> &RequestConfig {
        &self.config
    }

    /// Access the resolved model info.
    pub const fn model_info(&self) -> &ModelInfo {
        &self.model_info
    }

    /// Access the resolved API kind.
    pub const fn api_kind(&self) -> ApiKind {
        self.api_kind
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use test_support::SingleShotProvider;

    fn minimal_config() -> RequestConfig {
        RequestConfig::parse_yaml("model: test\n").expect("minimal config should parse")
    }

    fn test_model_info() -> ModelInfo {
        ModelInfo {
            provider: "test".into(),
            name: "test-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        }
    }

    #[test]
    fn new_constructs_with_mock_provider() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::stub(),
        );
        assert_eq!(client.config().model(), "test");
        assert_eq!(client.model_info().name, "test-model");
    }

    #[test]
    fn build_request_returns_valid_json() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::stub(),
        );
        let json = client.build_request("Hello, world!").unwrap();
        assert!(json.is_object());
        assert_eq!(json["model"], "test-model");
    }

    #[test]
    fn build_request_includes_tools_when_configured() {
        let config = RequestConfig::parse_yaml(
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
        .expect("config should parse");
        let client = FlickClient::new_with_provider(
            config,
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::stub(),
        );
        let json = client.build_request("read something").unwrap();
        assert!(json.is_object());
    }

    #[tokio::test]
    async fn run_returns_text_response() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::with_text("Hello back"),
        );
        let mut ctx = Context::default();
        let result = client.run("Hello", &mut ctx).await.unwrap();
        assert!(
            result
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "Hello back"))
        );
    }

    #[tokio::test]
    async fn run_pushes_user_message_to_context() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::with_text("reply"),
        );
        let mut ctx = Context::default();
        client.run("my query", &mut ctx).await.unwrap();
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, context::Role::User);
        assert_eq!(ctx.messages[1].role, context::Role::Assistant);
    }

    #[tokio::test]
    async fn resume_returns_result_after_tool_results() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::with_text("Done reading"),
        );
        let mut ctx = Context::default();
        ctx.push_user_text("read file").unwrap();
        ctx.push_assistant(vec![ContentBlock::ToolUse {
            id: "tc_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/test"}),
        }])
        .unwrap();

        let tool_results = vec![ContentBlock::ToolResult {
            tool_use_id: "tc_1".into(),
            content: "file contents".into(),
            is_error: false,
        }];
        let result = client.resume(&mut ctx, tool_results).await.unwrap();
        assert!(
            result
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "Done reading"))
        );
    }

    #[tokio::test]
    async fn run_with_tool_calls_returns_pending() {
        let client = FlickClient::new_with_provider(
            minimal_config(),
            test_model_info(),
            ApiKind::Messages,
            SingleShotProvider::with_tool_calls(vec![provider::ToolCallResponse {
                call_id: "tc_1".into(),
                tool_name: "read_file".into(),
                arguments: r#"{"path":"/tmp"}"#.into(),
            }]),
        );
        let mut ctx = Context::default();
        let result = client.run("read it", &mut ctx).await.unwrap();
        assert_eq!(result.status, result::ResultStatus::ToolCallsPending);
    }
}
