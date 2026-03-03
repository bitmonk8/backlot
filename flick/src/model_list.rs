use std::pin::Pin;

use crate::ApiKind;
use crate::error::FlickError;

/// Metadata about a model fetched from a provider's model list endpoint.
pub struct FetchedModel {
    pub id: String,
    pub max_completion_tokens: Option<u32>,
    pub context_length: Option<u32>,
}

/// Trait for fetching available models from a provider. Object-safe.
pub trait ModelFetcher: Send + Sync {
    fn fetch_models<'a>(
        &'a self,
        base_url: &'a str,
        api_key: &'a str,
        api: ApiKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<FetchedModel>, FlickError>> + Send + 'a>>;
}

/// Production model fetcher using HTTP.
pub struct HttpModelFetcher {
    client: reqwest::Client,
}

impl Default for HttpModelFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpModelFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl ModelFetcher for HttpModelFetcher {
    fn fetch_models<'a>(
        &'a self,
        base_url: &'a str,
        api_key: &'a str,
        api: ApiKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<FetchedModel>, FlickError>> + Send + 'a>> {
        Box::pin(async move {
            let url = format!("{}/v1/models", base_url.trim_end_matches('/'));

            let request = match api {
                ApiKind::Messages => self
                    .client
                    .get(&url)
                    .header("x-api-key", api_key)
                    .header("anthropic-version", "2023-06-01"),
                ApiKind::ChatCompletions => self
                    .client
                    .get(&url)
                    .header("Authorization", format!("Bearer {api_key}")),
            };

            let response = request
                .send()
                .await
                .map_err(|e| FlickError::Io(std::io::Error::other(format!("model fetch: {e}"))))?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                return Err(FlickError::Io(std::io::Error::other(format!(
                    "model fetch ({status}): {body}"
                ))));
            }

            let body: serde_json::Value = response
                .json()
                .await
                .map_err(|e| FlickError::Io(std::io::Error::other(format!("model fetch parse: {e}"))))?;

            let data = body
                .get("data")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    FlickError::Io(std::io::Error::other(
                        "model response missing 'data' array",
                    ))
                })?;

            let mut models = Vec::new();
            for item in data {
                if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
                    let max_completion_tokens = item
                        .get("top_provider")
                        .and_then(|tp| tp.get("max_completion_tokens"))
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok());
                    let context_length = item
                        .get("context_length")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok());
                    models.push(FetchedModel {
                        id: id.to_string(),
                        max_completion_tokens,
                        context_length,
                    });
                }
            }

            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(models)
        })
    }
}

/// Test model fetcher with pre-programmed response.
pub struct MockModelFetcher {
    result: std::sync::Mutex<Option<Result<Vec<FetchedModel>, FlickError>>>,
}

impl MockModelFetcher {
    pub fn with_models(models: Vec<FetchedModel>) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(Ok(models))),
        }
    }

    pub fn with_error(err: FlickError) -> Self {
        Self {
            result: std::sync::Mutex::new(Some(Err(err))),
        }
    }
}

impl ModelFetcher for MockModelFetcher {
    fn fetch_models<'a>(
        &'a self,
        _base_url: &'a str,
        _api_key: &'a str,
        _api: ApiKind,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<FetchedModel>, FlickError>> + Send + 'a>>
    {
        let result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| {
                Err(FlickError::Io(std::io::Error::other(
                    "MockModelFetcher: no more responses",
                )))
            });
        Box::pin(async move { result })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_with_models_returns_models() {
        let models = vec![
            FetchedModel {
                id: "model-a".into(),
                max_completion_tokens: Some(4096),
                context_length: Some(128_000),
            },
            FetchedModel {
                id: "model-b".into(),
                max_completion_tokens: None,
                context_length: None,
            },
        ];
        let fetcher = MockModelFetcher::with_models(models);
        let result = fetcher
            .fetch_models("http://example.com", "key", ApiKind::Messages)
            .await;
        let models = result.expect("should succeed");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "model-a");
        assert_eq!(models[0].max_completion_tokens, Some(4096));
    }

    #[tokio::test]
    async fn mock_with_error_returns_error() {
        let fetcher =
            MockModelFetcher::with_error(FlickError::Io(std::io::Error::other("test error")));
        let result = fetcher
            .fetch_models("http://example.com", "key", ApiKind::Messages)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn http_fetcher_messages_api() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"id": "claude-sonnet-4-20250514", "type": "model"},
                    {"id": "claude-opus-4-20250514", "type": "model"},
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let models = fetcher
            .fetch_models(&server.uri(), "test-key", ApiKind::Messages)
            .await
            .expect("should fetch");

        assert_eq!(models.len(), 2);
        // Sorted alphabetically
        assert_eq!(models[0].id, "claude-opus-4-20250514");
        assert_eq!(models[1].id, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn http_fetcher_chat_completions_api_with_metadata() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("Authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {
                        "id": "gpt-4o",
                        "context_length": 128_000,
                        "top_provider": {"max_completion_tokens": 16_384}
                    },
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let models = fetcher
            .fetch_models(&server.uri(), "test-key", ApiKind::ChatCompletions)
            .await
            .expect("should fetch");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-4o");
        assert_eq!(models[0].max_completion_tokens, Some(16_384));
        assert_eq!(models[0].context_length, Some(128_000));
    }

    #[tokio::test]
    async fn http_fetcher_error_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let result = fetcher
            .fetch_models(&server.uri(), "bad-key", ApiKind::Messages)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn http_fetcher_strips_trailing_slash() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "model-1"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let url_with_slash = format!("{}/", server.uri());
        let models = fetcher
            .fetch_models(&url_with_slash, "key", ApiKind::Messages)
            .await
            .expect("should fetch");
        assert_eq!(models.len(), 1);
    }

    #[tokio::test]
    async fn http_fetcher_empty_data_array() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let models = fetcher
            .fetch_models(&server.uri(), "key", ApiKind::Messages)
            .await
            .expect("should succeed");
        assert!(models.is_empty());
    }

    #[tokio::test]
    async fn http_fetcher_missing_data_field() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": []})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let fetcher = HttpModelFetcher::new();
        let result = fetcher
            .fetch_models(&server.uri(), "key", ApiKind::Messages)
            .await;
        assert!(result.is_err());
    }
}
