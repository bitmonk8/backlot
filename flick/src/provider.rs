pub mod messages;
pub mod chat_completions;
pub mod http;

use std::pin::Pin;

use crate::config::ProviderConfig;
use crate::context::Message;
use crate::error::ProviderError;
use crate::model::ReasoningLevel;

/// Parameters for a provider request.
pub struct RequestParams<'a> {
    pub model: &'a str,
    /// Maximum *output* tokens (not context window). Matches API field name.
    pub max_tokens: Option<u32>,
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

/// Complete response from a model provider.
pub struct ModelResponse {
    pub text: Option<String>,
    pub thinking: Vec<ThinkingContent>,
    pub tool_calls: Vec<ToolCallResponse>,
    pub usage: UsageResponse,
    pub warnings: Vec<Warning>,
}

/// A single thinking block from the response.
pub struct ThinkingContent {
    pub text: String,
    pub signature: String,
}

/// A single tool call from the response.
pub struct ToolCallResponse {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
}

/// Token usage from a single response.
#[derive(Default)]
pub struct UsageResponse {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// Non-fatal warning from the provider (e.g. `max_tokens` truncation).
pub struct Warning {
    pub message: String,
    pub code: String,
}

/// Provider trait — two methods: call and `build_request`.
pub trait Provider: Send + Sync {
    /// Call the model and return a complete response.
    fn call(
        &self,
        params: RequestParams<'_>,
    ) -> impl std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send;

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

impl DynProvider for ProviderInstance {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>
    {
        match self {
            Self::Messages(p) => Box::pin(p.call(params)),
            Self::ChatCompletions(p) => Box::pin(p.call(params)),
        }
    }

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError> {
        match self {
            Self::Messages(p) => Provider::build_request(p, params),
            Self::ChatCompletions(p) => Provider::build_request(p, params),
        }
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

/// Construct a provider from config. `base_url` comes from the credential store.
pub fn create_provider(
    provider_config: &ProviderConfig,
    api_key: String,
    base_url: &str,
) -> ProviderInstance {
    match provider_config.api {
        crate::ApiKind::Messages => {
            ProviderInstance::Messages(messages::MessagesProvider::new(base_url, api_key))
        }
        crate::ApiKind::ChatCompletions => {
            let compat = provider_config.compat.clone().unwrap_or_default();
            ProviderInstance::ChatCompletions(chat_completions::ChatCompletionsProvider::new(
                base_url, api_key, compat,
            ))
        }
    }
}

/// Object-safe wrapper for Provider.
pub trait DynProvider: Send + Sync {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>;

    fn build_request(
        &self,
        params: RequestParams<'_>,
    ) -> Result<serde_json::Value, ProviderError>;
}

impl<T: Provider> DynProvider for T {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>> {
        Box::pin(self.call(params))
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

    /// Single-user-message + empty-tools pair for `build_body` tests.
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
    use crate::ApiKind;
    use crate::config::{CompatFlags, ProviderConfig};

    #[test]
    fn create_provider_messages_variant() {
        let config = ProviderConfig {
            api: ApiKind::Messages,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&config, "test-key".into(), "https://custom.anthropic.com");
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
            credential: None,
            compat: Some(CompatFlags {
                explicit_tool_choice_auto: true,
            }),
        };
        let provider = create_provider(&config, "test-key".into(), "https://custom.openai.com");
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert_eq!(p.base_url(), "https://custom.openai.com");
                assert!(p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions variant"),
        }
    }

    #[test]
    fn create_provider_base_url_passed_through() {
        let messages_config = ProviderConfig {
            api: ApiKind::Messages,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&messages_config, "key".into(), "https://api.anthropic.com");
        match &provider {
            ProviderInstance::Messages(p) => {
                assert_eq!(p.base_url(), "https://api.anthropic.com");
            }
            ProviderInstance::ChatCompletions(_) => panic!("expected Messages"),
        }

        let openai_config = ProviderConfig {
            api: ApiKind::ChatCompletions,
            credential: None,
            compat: None,
        };
        let provider = create_provider(&openai_config, "key".into(), "https://api.openai.com");
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
            credential: None,
            compat: None,
        };
        let provider = create_provider(&config, "key".into(), "https://api.openai.com");
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert!(!p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions"),
        }
    }
}
