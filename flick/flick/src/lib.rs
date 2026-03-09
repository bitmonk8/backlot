pub mod config;
pub mod context;
pub mod credential;
pub mod error;
pub mod history;
pub mod model;
pub mod model_list;
pub mod provider;
pub mod result;
pub mod runner;

#[cfg(any(test, feature = "testing"))]
pub mod test_support;

use std::time::Duration;

use serde::{Deserialize, Serialize};

// Re-exports for convenience — library consumers can use `flick::Config` etc.
pub use config::{Config, ConfigFormat};
pub use context::{ContentBlock, Context, Message};
pub use credential::CredentialStore;
pub use error::FlickError;
pub use provider::DynProvider;
pub use result::FlickResult;

use provider::create_provider;

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

/// Resolve credentials from the default store and build a boxed provider.
///
/// This is the standard way to construct a provider for `FlickClient`.
/// Library callers who manage their own secrets can call `create_provider`
/// directly via `flick::provider::create_provider`.
pub async fn resolve_provider(config: &Config) -> Result<Box<dyn DynProvider>, FlickError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| FlickError::Io(std::io::Error::other(e.to_string())))?;
    let provider_config = config.active_provider()?;
    let cred_store = CredentialStore::new()?;
    let cred_name = provider_config
        .credential
        .as_deref()
        .unwrap_or_else(|| config.model().provider());
    let entry = cred_store.get(cred_name).await?;
    let provider = create_provider(provider_config, entry.key, &entry.base_url, client);
    Ok(Box::new(provider))
}

/// Reusable handle holding config and an injected provider.
/// Provider is always caller-supplied, keeping `FlickClient` fully testable.
pub struct FlickClient {
    config: Config,
    provider: Box<dyn DynProvider>,
}

impl FlickClient {
    /// Build from a config and an injected provider.
    ///
    /// Use `resolve_provider` to construct a provider from the credential store,
    /// or inject a mock for testing.
    pub fn new(config: Config, provider: Box<dyn DynProvider>) -> Self {
        Self { config, provider }
    }

    /// Single-shot query. Pushes the query as a user message, makes one model
    /// call, and returns the result. The caller owns and passes the `Context`.
    pub async fn run(&self, query: &str, context: &mut Context) -> Result<FlickResult, FlickError> {
        context.push_user_text(query)?;
        runner::run(&self.config, self.provider.as_ref(), context).await
    }

    /// Resume a session with tool results. Pushes the tool results, makes one
    /// model call, and returns the result.
    pub async fn resume(
        &self,
        context: &mut Context,
        tool_results: Vec<ContentBlock>,
    ) -> Result<FlickResult, FlickError> {
        context.push_tool_results(tool_results)?;
        runner::run(&self.config, self.provider.as_ref(), context).await
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
        let params = runner::build_params(&self.config, &context.messages, &tool_defs);
        self.provider
            .build_request(params)
            .map_err(FlickError::Provider)
    }

    /// Access the underlying config.
    pub const fn config(&self) -> &Config {
        &self.config
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use test_support::SingleShotProvider;

    fn minimal_config() -> Config {
        Config::parse_yaml(
            r"
model:
  provider: test
  name: test-model
  max_tokens: 1024

provider:
  test:
    api: messages
",
        )
        .expect("minimal config should parse")
    }

    #[test]
    fn new_constructs_with_mock_provider() {
        let client = FlickClient::new(minimal_config(), SingleShotProvider::stub());
        assert_eq!(client.config().model().name(), "test-model");
    }

    #[test]
    fn config_returns_the_config_passed_in() {
        let client = FlickClient::new(minimal_config(), SingleShotProvider::stub());
        assert_eq!(client.config().model().name(), "test-model");
        assert_eq!(client.config().model().provider(), "test");
        assert_eq!(client.config().model().max_tokens(), Some(1024));
    }

    #[test]
    fn build_request_returns_valid_json() {
        let client = FlickClient::new(minimal_config(), SingleShotProvider::stub());
        let json = client.build_request("Hello, world!").unwrap();
        assert!(json.is_object());
        assert_eq!(json["model"], "test-model");
    }

    #[test]
    fn build_request_includes_tools_when_configured() {
        let config = Config::parse_yaml(
            r"
model:
  provider: test
  name: test-model
  max_tokens: 1024

provider:
  test:
    api: messages

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
        let provider = SingleShotProvider::stub();
        let client = FlickClient::new(config, provider);
        let json = client.build_request("read something").unwrap();
        assert!(json.is_object());
    }

    #[tokio::test]
    async fn run_returns_text_response() {
        let client = FlickClient::new(
            minimal_config(),
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
        let client = FlickClient::new(minimal_config(), SingleShotProvider::with_text("reply"));
        let mut ctx = Context::default();
        client.run("my query", &mut ctx).await.unwrap();
        // Context should have user message + assistant message
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, context::Role::User);
        assert_eq!(ctx.messages[1].role, context::Role::Assistant);
    }

    #[tokio::test]
    async fn resume_returns_result_after_tool_results() {
        let client = FlickClient::new(
            minimal_config(),
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
        let client = FlickClient::new(
            minimal_config(),
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

    #[test]
    fn parse_fails_for_unknown_provider() {
        let yaml = r"
model:
  provider: nonexistent
  name: test-model

provider:
  anthropic:
    api: messages
";
        let result = Config::parse_yaml(yaml);
        assert!(result.is_err());
    }
}
