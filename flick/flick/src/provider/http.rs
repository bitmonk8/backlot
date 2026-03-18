// Re-uses OsRng from chacha20poly1305 (already a dependency) for backoff jitter.
// Crypto-grade randomness is unnecessary here but avoids adding a second RNG crate.
use chacha20poly1305::aead::rand_core::{OsRng, RngCore};

use crate::error::ProviderError;

/// Parse `Retry-After` header value into milliseconds.
/// Supports integer seconds format (most common for API rate limits).
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let val = headers.get("retry-after")?.to_str().ok()?;
    let secs: u64 = val.trim().parse().ok()?;
    Some(secs * 1000)
}

/// Map HTTP error status codes to `ProviderError`.
pub fn handle_http_error(status: u16, body: String, retry_after_ms: Option<u64>) -> ProviderError {
    match status {
        401 | 403 => ProviderError::AuthFailed,
        429 => ProviderError::RateLimited { retry_after_ms },
        _ => ProviderError::Api {
            status,
            message: body,
        },
    }
}

pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
        }
    }
}

pub(crate) enum RetryVerdict {
    Retry { delay_ms: Option<u64> },
    Fail,
}

pub(crate) const fn classify_for_retry(err: &ProviderError) -> RetryVerdict {
    match err {
        ProviderError::Http(_) => RetryVerdict::Retry { delay_ms: None },
        ProviderError::RateLimited { retry_after_ms } => RetryVerdict::Retry {
            delay_ms: *retry_after_ms,
        },
        ProviderError::Api { status, .. } if *status >= 500 || *status == 408 => {
            RetryVerdict::Retry { delay_ms: None }
        }
        ProviderError::AuthFailed
        | ProviderError::ResponseParse(_)
        | ProviderError::InvalidRequest(_)
        | ProviderError::Api { .. } => RetryVerdict::Fail,
    }
}

/// Send an HTTP request with retry and exponential backoff.
///
/// Returns the successful response. The caller is responsible for reading the
/// body and parsing the JSON.
pub async fn send_with_retry<F>(
    policy: &RetryPolicy,
    mut build_request: F,
) -> Result<reqwest::Response, ProviderError>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut attempt = 0u32;
    let mut backoff_ms = policy.initial_backoff_ms;

    loop {
        let result = build_request().send().await;

        match result {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let retry_after = parse_retry_after(response.headers());
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                let err = handle_http_error(status, body, retry_after);

                if attempt >= policy.max_retries {
                    return Err(err);
                }

                match classify_for_retry(&err) {
                    RetryVerdict::Retry { delay_ms } => {
                        let delay = delay_ms.unwrap_or_else(|| {
                            let jitter = OsRng.next_u64() % (backoff_ms / 2 + 1);
                            backoff_ms + jitter
                        });
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                        backoff_ms = (backoff_ms * 2).min(policy.max_backoff_ms);
                        attempt += 1;
                    }
                    RetryVerdict::Fail => return Err(err),
                }
            }
            Err(e) => {
                if attempt >= policy.max_retries {
                    return Err(ProviderError::Http(e));
                }

                let jitter = OsRng.next_u64() % (backoff_ms / 2 + 1);
                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms + jitter)).await;
                backoff_ms = (backoff_ms * 2).min(policy.max_backoff_ms);
                attempt += 1;
            }
        }
    }
}

/// Send a request with retry, read the full body, and parse as JSON.
pub async fn request_json(
    build_request: impl FnMut() -> reqwest::RequestBuilder,
) -> Result<serde_json::Value, ProviderError> {
    let response = send_with_retry(&RetryPolicy::default(), build_request).await?;
    let body = response.text().await.map_err(ProviderError::Http)?;
    serde_json::from_str(&body).map_err(|e| ProviderError::ResponseParse(format!("{e}: {body}")))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn handle_http_error_401() {
        let err = handle_http_error(401, "unauthorized".into(), None);
        assert!(matches!(err, ProviderError::AuthFailed));
    }

    #[test]
    fn handle_http_error_403() {
        let err = handle_http_error(403, "forbidden".into(), None);
        assert!(matches!(err, ProviderError::AuthFailed));
    }

    #[test]
    fn handle_http_error_429_no_retry() {
        let err = handle_http_error(429, String::new(), None);
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_ms: None
            }
        ));
    }

    #[test]
    fn handle_http_error_429_with_retry() {
        let err = handle_http_error(429, String::new(), Some(5000));
        match err {
            ProviderError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, Some(5000));
            }
            _ => panic!("expected RateLimited"),
        }
    }

    #[test]
    fn handle_http_error_500() {
        let err = handle_http_error(500, "internal error".into(), None);
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "internal error");
            }
            _ => panic!("expected Api error"),
        }
    }

    #[test]
    fn parse_retry_after_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "5".parse().expect("valid header"));
        assert_eq!(parse_retry_after(&headers), Some(5000));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_non_numeric() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "not-a-number".parse().expect("valid header"));
        assert_eq!(parse_retry_after(&headers), None);
    }

    // -- classify_for_retry tests --

    #[tokio::test]
    async fn classify_http_error_is_retryable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}"))
            .send()
            .await
            .expect_err("port is closed");
        let provider_err = ProviderError::Http(err);
        assert!(matches!(
            classify_for_retry(&provider_err),
            RetryVerdict::Retry { delay_ms: None }
        ));
    }

    #[test]
    fn classify_rate_limited_with_retry_after() {
        let err = ProviderError::RateLimited {
            retry_after_ms: Some(5000),
        };
        assert!(matches!(
            classify_for_retry(&err),
            RetryVerdict::Retry {
                delay_ms: Some(5000)
            }
        ));
    }

    #[test]
    fn classify_rate_limited_without_retry_after() {
        let err = ProviderError::RateLimited {
            retry_after_ms: None,
        };
        assert!(matches!(
            classify_for_retry(&err),
            RetryVerdict::Retry { delay_ms: None }
        ));
    }

    #[test]
    fn classify_server_error_is_retryable() {
        let err = ProviderError::Api {
            status: 500,
            message: "internal".into(),
        };
        assert!(matches!(
            classify_for_retry(&err),
            RetryVerdict::Retry { delay_ms: None }
        ));
    }

    #[test]
    fn classify_client_error_is_not_retryable() {
        let err = ProviderError::Api {
            status: 400,
            message: "bad request".into(),
        };
        assert!(matches!(classify_for_retry(&err), RetryVerdict::Fail));
    }

    #[test]
    fn classify_auth_failed_is_not_retryable() {
        assert!(matches!(
            classify_for_retry(&ProviderError::AuthFailed),
            RetryVerdict::Fail
        ));
    }

    #[test]
    fn classify_response_parse_is_not_retryable() {
        let err = ProviderError::ResponseParse("bad data".into());
        assert!(matches!(classify_for_retry(&err), RetryVerdict::Fail));
    }

    #[test]
    fn classify_invalid_request_is_not_retryable() {
        let err = ProviderError::InvalidRequest("empty messages".into());
        assert!(matches!(classify_for_retry(&err), RetryVerdict::Fail));
    }

    #[test]
    fn handle_http_error_408() {
        let err = handle_http_error(408, "request timeout".into(), None);
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 408);
                assert_eq!(message, "request timeout");
            }
            _ => panic!("expected Api error"),
        }
    }

    #[test]
    fn classify_request_timeout_is_retryable() {
        let err = ProviderError::Api {
            status: 408,
            message: "request timeout".into(),
        };
        assert!(matches!(
            classify_for_retry(&err),
            RetryVerdict::Retry { delay_ms: None }
        ));
    }

    // -- send_with_retry tests --

    fn fast_retry_policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            initial_backoff_ms: 1,
            max_backoff_ms: 10,
        }
    }

    #[tokio::test]
    async fn send_with_retry_success_first_try() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let response = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn send_with_retry_retries_on_500_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let response = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn send_with_retry_exhausts_retries_on_persistent_500() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .expect(4)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        match result {
            Err(ProviderError::Api { status: 500, .. }) => {}
            other => panic!("expected Api 500, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_with_retry_no_retry_on_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(matches!(result, Err(ProviderError::AuthFailed)));
    }

    #[tokio::test]
    async fn send_with_retry_429_retries_with_retry_after() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("rate limited")
                    .append_header("retry-after", "0"),
            )
            .up_to_n_times(1)
            .expect(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_with_retry_stops_on_non_retryable_after_retryable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(matches!(result, Err(ProviderError::AuthFailed)));
    }

    #[tokio::test]
    async fn request_json_parses_valid_json() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = request_json(|| client.post(&url)).await;
        let value = result.expect("should parse");
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn request_json_returns_parse_error_on_invalid_json() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        let result = request_json(|| client.post(&url)).await;
        assert!(matches!(result, Err(ProviderError::ResponseParse(_))));
    }
}
