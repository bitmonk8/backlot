pub mod messages;
pub mod chat_completions;
pub mod sse;

use std::pin::Pin;
use tokio_stream::Stream;

use crate::config::ProviderConfig;
use crate::context::Message;
use crate::error::ProviderError;
use crate::event::StreamEvent;
use crate::model::ReasoningLevel;

/// Parameters for a provider request.
pub struct RequestParams<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub system_prompt: Option<&'a str>,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDefinition],
    pub reasoning: Option<ReasoningLevel>,
    pub output_schema: Option<&'a serde_json::Value>,
}

/// Tool definition sent to the model.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

pub type EventStream =
    Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// Provider trait — two methods: stream and `build_request`.
pub trait Provider: Send + Sync {
    /// Stream events from the model.
    fn stream(
        &self,
        params: RequestParams<'_>,
    ) -> impl std::future::Future<Output = Result<EventStream, ProviderError>> + Send;

    /// Build the request body as JSON (for --dry-run).
    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError>;
}

/// Concrete provider enum — enables test verification of constructed variant.
pub enum ProviderInstance {
    Messages(messages::MessagesProvider),
    ChatCompletions(chat_completions::ChatCompletionsProvider),
}

impl ProviderInstance {
    fn inner(&self) -> &dyn DynProvider {
        match self {
            Self::Messages(p) => p,
            Self::ChatCompletions(p) => p,
        }
    }
}

impl DynProvider for ProviderInstance {
    fn stream_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>>
    {
        self.inner().stream_boxed(params)
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        self.inner().build_request(params)
    }
}

/// Extract an (`input_tokens`, `output_tokens`) pair from a JSON usage object.
/// Returns `None` if neither field is present.
pub(crate) fn extract_token_pair(
    usage: &serde_json::Value,
    input_field: &str,
    output_field: &str,
) -> Option<(u64, u64)> {
    let input = usage.get(input_field).and_then(serde_json::Value::as_u64);
    let output = usage.get(output_field).and_then(serde_json::Value::as_u64);
    if input.is_some() || output.is_some() {
        Some((input.unwrap_or(0), output.unwrap_or(0)))
    } else {
        None
    }
}

/// Construct a provider from config.
pub fn create_provider(
    provider_config: &ProviderConfig,
    api_key: String,
) -> ProviderInstance {
    match provider_config.api {
        crate::config::ApiKind::Messages => {
            let base_url = provider_config
                .base_url
                .clone()
                .unwrap_or_else(|| messages::DEFAULT_BASE_URL.to_string());
            ProviderInstance::Messages(messages::MessagesProvider::new(base_url, api_key))
        }
        crate::config::ApiKind::ChatCompletions => {
            let base_url = provider_config
                .base_url
                .clone()
                .unwrap_or_else(|| chat_completions::DEFAULT_BASE_URL.to_string());
            let compat = provider_config.compat.clone().unwrap_or_default();
            ProviderInstance::ChatCompletions(chat_completions::ChatCompletionsProvider::new(
                base_url, api_key, compat,
            ))
        }
    }
}

/// Object-safe wrapper for Provider.
pub trait DynProvider: Send + Sync {
    fn stream_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>>;

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError>;
}

impl<T: Provider> DynProvider for T {
    fn stream_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<EventStream, ProviderError>> + Send + 'a>> {
        Box::pin(self.stream(params))
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        Provider::build_request(self, params)
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::context::{ContentBlock, Message, Role};
    use crate::provider::ToolDefinition;

    /// Wraps string slices into an SSE-like byte stream (shared by both provider
    /// test modules).
    pub fn byte_stream(
        chunks: Vec<&str>,
    ) -> impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static
    {
        let owned: Vec<_> = chunks
            .into_iter()
            .map(|s| Ok(bytes::Bytes::from(s.to_owned())))
            .collect();
        tokio_stream::iter(owned)
    }

    /// Single-user-message + empty-tools pair for build_body tests.
    pub fn minimal_params() -> (Vec<Message>, Vec<ToolDefinition>) {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        }];
        (msgs, vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKind, CompatFlags, ProviderConfig};

    #[test]
    fn create_provider_messages_variant() {
        let config = ProviderConfig {
            api: ApiKind::Messages,
            base_url: Some("https://custom.anthropic.com".into()),
            credential: None,
            compat: None,
        };
        let provider = create_provider(&config, "test-key".into());
        match &provider {
            ProviderInstance::Messages(p) => {
                assert_eq!(p.base_url(), "https://custom.anthropic.com");
            }
            ProviderInstance::ChatCompletions(_) => panic!("expected Messages variant"),
        }
    }

    #[test]
    fn create_provider_chat_completions_variant() {
        let config = ProviderConfig {
            api: ApiKind::ChatCompletions,
            base_url: Some("https://custom.openai.com".into()),
            credential: None,
            compat: Some(CompatFlags {
                explicit_tool_choice_auto: true,
                ..CompatFlags::default()
            }),
        };
        let provider = create_provider(&config, "test-key".into());
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert_eq!(p.base_url(), "https://custom.openai.com");
                assert!(p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions variant"),
        }
    }

    #[test]
    fn create_provider_default_base_urls() {
        let messages_config = ProviderConfig {
            api: ApiKind::Messages,
            base_url: None,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&messages_config, "key".into());
        match &provider {
            ProviderInstance::Messages(p) => {
                assert_eq!(p.base_url(), "https://api.anthropic.com");
            }
            ProviderInstance::ChatCompletions(_) => panic!("expected Messages"),
        }

        let openai_config = ProviderConfig {
            api: ApiKind::ChatCompletions,
            base_url: None,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&openai_config, "key".into());
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert_eq!(p.base_url(), "https://api.openai.com");
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions"),
        }
    }

    #[test]
    fn create_provider_chat_completions_default_flags() {
        let config = ProviderConfig {
            api: ApiKind::ChatCompletions,
            base_url: None,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&config, "key".into());
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert!(!p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions"),
        }
    }
}
