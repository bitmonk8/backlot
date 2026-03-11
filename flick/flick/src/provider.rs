pub mod chat_completions;
pub mod http;
pub mod messages;

use std::pin::Pin;

use crate::context::Message;
use crate::error::ProviderError;
use crate::model::ReasoningLevel;
use crate::provider_registry::ProviderInfo;

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
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Option<serde_json::Value>,
}

/// Complete response from a model provider.
pub struct ModelResponse {
    pub text: Option<String>,
    pub thinking: Vec<ThinkingContent>,
    pub tool_calls: Vec<ToolCallResponse>,
    pub usage: UsageResponse,
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
            Self::Messages(p) => p.call_boxed(params),
            Self::ChatCompletions(p) => p.call_boxed(params),
        }
    }

    fn build_request(&self, params: RequestParams<'_>) -> Result<serde_json::Value, ProviderError> {
        match self {
            Self::Messages(p) => DynProvider::build_request(p, params),
            Self::ChatCompletions(p) => DynProvider::build_request(p, params),
        }
    }
}

/// Construct a provider from resolved registry info.
pub fn create_provider(
    provider_info: &ProviderInfo,
    client: reqwest::Client,
) -> ProviderInstance {
    match provider_info.api {
        crate::ApiKind::Messages => {
            ProviderInstance::Messages(messages::MessagesProvider::new(
                &provider_info.base_url,
                provider_info.key.clone(),
                client,
            ))
        }
        crate::ApiKind::ChatCompletions => {
            let compat = provider_info.compat.clone().unwrap_or_default();
            ProviderInstance::ChatCompletions(chat_completions::ChatCompletionsProvider::new(
                &provider_info.base_url,
                provider_info.key.clone(),
                compat,
                client,
            ))
        }
    }
}

/// Object-safe provider trait — implemented directly by each provider.
pub trait DynProvider: Send + Sync {
    fn call_boxed<'a>(
        &'a self,
        params: RequestParams<'a>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, ProviderError>> + Send + 'a>>;

    fn build_request(&self, params: RequestParams<'_>) -> Result<serde_json::Value, ProviderError>;
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
    use crate::config::CompatFlags;

    #[test]
    fn create_provider_messages_variant() {
        let info = ProviderInfo {
            api: ApiKind::Messages,
            base_url: "https://custom.anthropic.com".into(),
            key: "test-key".into(),
            compat: None,
        };
        let provider = create_provider(&info, reqwest::Client::new());
        match &provider {
            ProviderInstance::Messages(p) => {
                assert_eq!(p.base_url(), "https://custom.anthropic.com");
            }
            ProviderInstance::ChatCompletions(_) => panic!("expected Messages variant"),
        }
    }

    #[test]
    fn create_provider_chat_completions_variant() {
        let info = ProviderInfo {
            api: ApiKind::ChatCompletions,
            base_url: "https://custom.openai.com".into(),
            key: "test-key".into(),
            compat: Some(CompatFlags {
                explicit_tool_choice_auto: true,
            }),
        };
        let provider = create_provider(&info, reqwest::Client::new());
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert_eq!(p.base_url(), "https://custom.openai.com");
                assert!(p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions variant"),
        }
    }

    #[test]
    fn create_provider_chat_completions_default_flags() {
        let info = ProviderInfo {
            api: ApiKind::ChatCompletions,
            base_url: "https://api.openai.com".into(),
            key: "key".into(),
            compat: None,
        };
        let provider = create_provider(&info, reqwest::Client::new());
        match &provider {
            ProviderInstance::ChatCompletions(p) => {
                assert!(!p.compat().explicit_tool_choice_auto);
            }
            ProviderInstance::Messages(_) => panic!("expected ChatCompletions"),
        }
    }
}
