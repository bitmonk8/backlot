// Re-uses OsRng from chacha20poly1305 (already a dependency) for backoff jitter.
// Crypto-grade randomness is unnecessary here but avoids adding a second RNG crate.
use chacha20poly1305::aead::rand_core::{OsRng, RngCore};
use smallvec::SmallVec;

use crate::error::ProviderError;
use crate::event::StreamEvent;

/// Maximum SSE buffer size (16 MiB). Protects against unbounded memory growth
/// from a misbehaving server that never sends double-newline delimiters.
const MAX_SSE_BUFFER_BYTES: usize = 16 * 1024 * 1024;

/// Default idle timeout for SSE streams (5 minutes). If no bytes arrive within
/// this duration, the stream is terminated with `ProviderError::StreamTimeout`.
pub const DEFAULT_SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Avoids heap allocation for the common 0-2 events per SSE block.
pub type EventBatch = SmallVec<[StreamEvent; 2]>;

/// Result from processing an SSE event block. `Done` signals end of stream.
/// `DoneWithEvents` emits final events then terminates (e.g., error events
/// that must be surfaced before the stream ends).
pub enum SseAction {
    Events(EventBatch),
    Done,
    DoneWithEvents(EventBatch),
}

/// Spawn an SSE parsing task that reads from a byte stream, splits on
/// double-newline boundaries, and calls the block handler for each block.
///
/// The handler receives raw SSE blocks (text between `\n\n` delimiters)
/// and returns events to emit or signals end of stream.
///
/// `idle_timeout` caps how long the parser waits for the next chunk of bytes.
/// If no bytes arrive within that duration, the stream is terminated with
/// `ProviderError::StreamTimeout`.
pub fn spawn_sse_parser<F>(
    byte_stream: impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
        + Send
        + 'static,
    idle_timeout: std::time::Duration,
    block_handler: F,
) -> impl tokio_stream::Stream<Item = Result<StreamEvent, ProviderError>> + Send
where
    F: FnMut(&str) -> Result<SseAction, ProviderError> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ProviderError>>(64);

    let tx_monitor = tx.clone();
    let handle = tokio::spawn(async move {
        use tokio_stream::StreamExt;

        let mut buffer = String::new();
        let mut byte_stream = std::pin::pin!(byte_stream);
        let mut handler = block_handler;
        let mut incomplete_utf8: Vec<u8> = Vec::new();

        loop {
            let next = tokio::time::timeout(idle_timeout, byte_stream.next()).await;
            let chunk_result = match next {
                Ok(Some(result)) => result,
                Ok(None) => break, // stream ended
                Err(_) => {
                    let _ = tx
                        .send(Err(ProviderError::StreamTimeout(idle_timeout.as_secs())))
                        .await;
                    return;
                }
            };
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::Http(e))).await;
                    return;
                }
            };

            // Buffer incomplete UTF-8 bytes across chunk boundaries
            let bytes = if incomplete_utf8.is_empty() {
                chunk.to_vec()
            } else {
                incomplete_utf8.extend_from_slice(&chunk);
                std::mem::take(&mut incomplete_utf8)
            };

            match std::str::from_utf8(&bytes) {
                Ok(s) => buffer.push_str(s),
                Err(e) => {
                    let valid_up_to = e.valid_up_to();
                    if let Ok(valid) = std::str::from_utf8(&bytes[..valid_up_to]) {
                        buffer.push_str(valid);
                    }
                    if e.error_len().is_none() {
                        // Incomplete multi-byte sequence at end — buffer for next chunk
                        incomplete_utf8.extend_from_slice(&bytes[valid_up_to..]);
                    } else {
                        // Genuinely invalid bytes — use lossy fallback for remainder
                        buffer.push_str(&String::from_utf8_lossy(&bytes[valid_up_to..]));
                    }
                }
            }

            if buffer.len() > MAX_SSE_BUFFER_BYTES {
                let _ = tx
                    .send(Err(ProviderError::SseParse(
                        "SSE buffer limit exceeded".into(),
                    )))
                    .await;
                return;
            }

            while let Some(pos) = buffer.find("\n\n") {
                // Extract the block text, then drain from buffer
                let block_text = buffer[..pos].to_string();
                buffer.drain(..pos + 2);

                match handler(&block_text) {
                    Ok(SseAction::Events(events)) => {
                        for event in events {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Ok(SseAction::Done) => return,
                    Ok(SseAction::DoneWithEvents(events)) => {
                        for event in events {
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        }

        // Non-empty buffer or incomplete UTF-8 at stream end means the connection dropped mid-event.
        if !buffer.trim().is_empty() || !incomplete_utf8.is_empty() {
            let _ = tx
                .send(Err(ProviderError::SseParse(
                    "stream ended with incomplete SSE block".into(),
                )))
                .await;
        }
    });

    // Monitor task: if the parser task panics, send an error through the channel
    // so the consumer sees a proper error instead of a silent end-of-stream.
    tokio::spawn(async move {
        if let Err(e) = handle.await {
            if e.is_panic() {
                let _ = tx_monitor
                    .send(Err(ProviderError::SseParse(
                        "SSE parser task panicked".into(),
                    )))
                    .await;
            }
        }
    });

    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// Parse an SSE block into `event_type` and data fields (Anthropic-style).
pub fn parse_event_data(block: &str) -> (Option<&str>, Option<&str>) {
    let mut event_type = None;
    let mut data = None;
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = Some(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    (event_type, data)
}

/// Parse `Retry-After` header value into milliseconds.
/// Supports integer seconds format (most common for API rate limits).
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let val = headers.get("retry-after")?.to_str().ok()?;
    let secs: u64 = val.trim().parse().ok()?;
    Some(secs * 1000)
}

/// Boxed byte stream — concrete type for passing reqwest streams through helpers.
pub type ByteStream =
    std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>;

/// Send a streaming request with retry, validate, and parse as SSE.
///
/// Encapsulates the shared HTTP plumbing common to all provider
/// `stream()` implementations.
pub async fn stream_request<S>(
    build_request: impl FnMut() -> reqwest::RequestBuilder,
    parse: impl FnOnce(ByteStream) -> S,
) -> Result<super::EventStream, ProviderError>
where
    S: tokio_stream::Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static,
{
    let response = send_with_retry(&RetryPolicy::default(), build_request).await?;
    validate_sse_content_type(&response)?;
    let byte_stream: ByteStream = Box::pin(response.bytes_stream());
    Ok(Box::pin(parse(byte_stream)))
}

/// Validate that the response Content-Type is `text/event-stream`.
///
/// Returns an error if a proxy or misconfigured server returns a different type
/// (e.g. `application/json`), which would produce garbage when parsed as SSE.
/// Missing Content-Type is tolerated (some proxies strip headers).
pub fn validate_sse_content_type(response: &reqwest::Response) -> Result<(), ProviderError> {
    if let Some(ct) = response.headers().get(reqwest::header::CONTENT_TYPE) {
        if let Ok(ct_str) = ct.to_str() {
            if !ct_str.starts_with("text/event-stream") {
                return Err(ProviderError::SseParse(format!(
                    "expected Content-Type text/event-stream, got {ct_str}"
                )));
            }
        }
    }
    Ok(())
}

/// Map HTTP error status codes to `ProviderError`.
pub fn handle_http_error(
    status: u16,
    body: String,
    retry_after_ms: Option<u64>,
) -> ProviderError {
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
        ProviderError::Api { status, .. } if *status >= 500 => {
            RetryVerdict::Retry { delay_ms: None }
        }
        ProviderError::AuthFailed
        | ProviderError::SseParse(_)
        | ProviderError::StreamTimeout(_)
        | ProviderError::StreamError(_)
        | ProviderError::Api { .. } => RetryVerdict::Fail,
    }
}

/// Send an HTTP request with retry and exponential backoff.
///
/// Only retries the initial request/response exchange. Once a successful
/// response is returned, the caller owns the byte stream and no further
/// retries are attempted (mid-stream retry is not viable because SSE events
/// have already been emitted to stdout).
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_event_data_anthropic_style() {
        let block = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}";
        let (event_type, data) = parse_event_data(block);
        assert_eq!(event_type, Some("content_block_delta"));
        assert_eq!(data, Some("{\"type\":\"content_block_delta\"}"));
    }

    #[test]
    fn parse_event_data_no_event_line() {
        let block = "data: {\"choices\":[]}";
        let (event_type, data) = parse_event_data(block);
        assert_eq!(event_type, None);
        assert_eq!(data, Some("{\"choices\":[]}"));
    }

    #[test]
    fn parse_event_data_empty_block() {
        let (event_type, data) = parse_event_data("");
        assert!(event_type.is_none());
        assert!(data.is_none());
    }

    #[test]
    fn parse_event_data_comment_lines_ignored() {
        let block = ": comment\nevent: message_start\ndata: {}";
        let (event_type, data) = parse_event_data(block);
        assert_eq!(event_type, Some("message_start"));
        assert_eq!(data, Some("{}"));
    }

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
            ProviderError::RateLimited { retry_after_ms: None }
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

    /// Helper: create a byte stream from a list of chunks.
    fn byte_stream(
        chunks: Vec<&str>,
    ) -> impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static
    {
        let owned: Vec<_> = chunks
            .into_iter()
            .map(|s| Ok(bytes::Bytes::from(s.to_owned())))
            .collect();
        tokio_stream::iter(owned)
    }

    /// Collect all events from a `spawn_sse_parser` stream.
    async fn collect_events<F>(
        chunks: Vec<&str>,
        handler: F,
    ) -> Vec<Result<StreamEvent, ProviderError>>
    where
        F: FnMut(&str) -> Result<SseAction, ProviderError> + Send + 'static,
    {
        use tokio_stream::StreamExt;
        let stream = spawn_sse_parser(byte_stream(chunks), DEFAULT_SSE_IDLE_TIMEOUT, handler);
        tokio::pin!(stream);
        let mut results = Vec::new();
        while let Some(item) = stream.next().await {
            results.push(item);
        }
        results
    }

    #[tokio::test]
    async fn spawn_sse_parser_single_block() {
        let results = collect_events(
            vec!["event: test\ndata: hello\n\n"],
            |block| {
                assert_eq!(block, "event: test\ndata: hello");
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta {
                        text: "hello".into(),
                    }
                ]))
            },
        )
        .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
    }

    #[tokio::test]
    async fn spawn_sse_parser_block_split_across_chunks() {
        let results = collect_events(
            vec!["event: test\n", "data: split\n\n"],
            |block| {
                assert_eq!(block, "event: test\ndata: split");
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta {
                        text: "split".into(),
                    }
                ]))
            },
        )
        .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
    }

    #[tokio::test]
    async fn spawn_sse_parser_multiple_blocks_in_one_chunk() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();
        let results = collect_events(
            vec!["data: first\n\ndata: second\n\n"],
            move |_block| {
                cc.fetch_add(1, Ordering::Relaxed);
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta {
                        text: "x".into(),
                    }
                ]))
            },
        )
        .await;
        assert_eq!(results.len(), 2);
        assert_eq!(call_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn spawn_sse_parser_done_stops_stream() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();
        let results = collect_events(
            vec!["data: first\n\ndata: second\n\n"],
            move |_block| {
                let n = cc.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    Ok(SseAction::Done)
                } else {
                    Ok(SseAction::Events(smallvec::smallvec![
                        StreamEvent::TextDelta {
                            text: "should not appear".into(),
                        }
                    ]))
                }
            },
        )
        .await;
        assert!(results.is_empty());
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn spawn_sse_parser_handler_error_propagates() {
        let results = collect_events(
            vec!["data: bad\n\n"],
            |_block| Err(ProviderError::SseParse("test error".into())),
        )
        .await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
        match &results[0] {
            Err(ProviderError::SseParse(msg)) => assert_eq!(msg, "test error"),
            other => panic!("expected SseParse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_sse_parser_empty_events_batch() {
        let results = collect_events(
            vec!["data: ignored\n\n"],
            |_block| Ok(SseAction::Events(smallvec::smallvec![])),
        )
        .await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn spawn_sse_parser_utf8_lossy_fallback() {
        // Invalid UTF-8 bytes — should use lossy conversion, not crash
        let invalid_chunk: Vec<u8> = b"data: hello\xff\n\n".to_vec();
        let stream = tokio_stream::iter(vec![Ok(bytes::Bytes::from(invalid_chunk))]);
        let results = {
            use tokio_stream::StreamExt;
            let stream = spawn_sse_parser(stream, DEFAULT_SSE_IDLE_TIMEOUT, |block| {
                assert!(block.contains("hello"));
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta { text: "ok".into() }
                ]))
            });
            tokio::pin!(stream);
            let mut r = Vec::new();
            while let Some(item) = stream.next().await {
                r.push(item);
            }
            r
        };
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
    }

    #[tokio::test]
    async fn spawn_sse_parser_chunk_boundary_utf8() {
        // Multi-byte UTF-8 character split across chunks: "é" = 0xC3 0xA9
        let chunk1 = bytes::Bytes::from(vec![b'd', b'a', b't', b'a', b':', b' ', 0xC3]);
        let chunk2 = bytes::Bytes::from(vec![0xA9, b'\n', b'\n']);
        let stream = tokio_stream::iter(vec![Ok(chunk1), Ok(chunk2)]);
        let results = {
            use tokio_stream::StreamExt;
            let stream = spawn_sse_parser(stream, DEFAULT_SSE_IDLE_TIMEOUT, |block| {
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta { text: block.to_string() }
                ]))
            });
            tokio::pin!(stream);
            let mut r = Vec::new();
            while let Some(item) = stream.next().await {
                r.push(item);
            }
            r
        };
        assert_eq!(results.len(), 1);
        assert!(results[0].is_ok());
        // Verify the multi-byte character was preserved across chunks
        if let Ok(StreamEvent::TextDelta { text }) = &results[0] {
            assert!(text.contains('\u{00E9}'), "should contain é (U+00E9)");
        } else {
            panic!("expected Ok(TextDelta)");
        }
    }

    #[test]
    fn parse_event_data_no_trailing_space() {
        let block = "data:no-space";
        let (event_type, data) = parse_event_data(block);
        assert!(event_type.is_none());
        assert_eq!(data, Some("no-space"));
    }

    #[tokio::test]
    async fn spawn_sse_parser_buffer_limit_exceeded() {
        // Create a chunk larger than MAX_SSE_BUFFER_BYTES without \n\n
        let big_chunk = "x".repeat(MAX_SSE_BUFFER_BYTES + 1);
        let stream = tokio_stream::iter(vec![Ok(bytes::Bytes::from(big_chunk))]);
        let results = {
            use tokio_stream::StreamExt;
            let stream = spawn_sse_parser(stream, DEFAULT_SSE_IDLE_TIMEOUT, |_block| {
                Ok(SseAction::Events(smallvec::smallvec![]))
            });
            tokio::pin!(stream);
            let mut r = Vec::new();
            while let Some(item) = stream.next().await {
                r.push(item);
            }
            r
        };
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
        match &results[0] {
            Err(ProviderError::SseParse(msg)) => assert!(msg.contains("buffer limit")),
            other => panic!("expected SseParse buffer limit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_sse_parser_incomplete_block_at_end() {
        // Block without trailing \n\n — should signal error (abnormal termination)
        let results = collect_events(
            vec!["data: no-terminator"],
            |_block| {
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta {
                        text: "x".into(),
                    }
                ]))
            },
        )
        .await;
        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(ProviderError::SseParse(msg)) => {
                assert!(msg.contains("incomplete SSE block"));
            }
            other => panic!("expected SseParse incomplete block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_sse_parser_idle_timeout() {
        use tokio_stream::StreamExt;

        // Stream that sends one chunk then stalls forever
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, reqwest::Error>>(4);
        chunk_tx
            .send(Ok(bytes::Bytes::from("data: hello\n\n")))
            .await
            .expect("send");
        // Don't close chunk_tx — simulates a stalled server

        let timeout = std::time::Duration::from_millis(50);
        let stream = spawn_sse_parser(
            tokio_stream::wrappers::ReceiverStream::new(chunk_rx),
            timeout,
            |_block| {
                Ok(SseAction::Events(smallvec::smallvec![
                    StreamEvent::TextDelta { text: "x".into() }
                ]))
            },
        );
        tokio::pin!(stream);

        // First event: the parsed text delta
        let first = stream.next().await;
        assert!(matches!(first, Some(Ok(StreamEvent::TextDelta { .. }))));

        // Second event: timeout error after ~50ms of silence
        let second = stream.next().await;
        match second {
            Some(Err(ProviderError::StreamTimeout(secs))) => assert_eq!(secs, 0),
            other => panic!("expected StreamTimeout, got {other:?}"),
        }

        // Stream should end after timeout
        assert!(stream.next().await.is_none());
    }

    // -- classify_for_retry tests --

    #[tokio::test]
    async fn classify_http_error_is_retryable() {
        // Bind a port then immediately close it to guarantee connection refused
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
    fn classify_sse_parse_is_not_retryable() {
        let err = ProviderError::SseParse("bad data".into());
        assert!(matches!(classify_for_retry(&err), RetryVerdict::Fail));
    }

    #[test]
    fn classify_stream_timeout_is_not_retryable() {
        let err = ProviderError::StreamTimeout(300);
        assert!(matches!(classify_for_retry(&err), RetryVerdict::Fail));
    }

    // -- send_with_retry tests --

    /// Small backoff policy for fast tests (no need for time mocking).
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
        // First request: 500 (retryable), high priority, once
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        // Fallback: 200 success (default priority)
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
            .expect(4) // 1 initial + 3 retries
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
            .expect(1) // no retries
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
        // First request: 429 with retry-after (high priority, once)
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
        // Fallback: 200 success (default priority)
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/test", server.uri());

        // retry-after: 0 makes the retry instant; classify_for_retry unit tests
        // verify that the retry_after_ms value is propagated correctly
        let result = send_with_retry(&fast_retry_policy(), || client.post(&url)).await;
        assert!(result.is_ok());
        // Mock expectations verify: 1 request hit 429, 1 request hit 200
    }

    #[tokio::test]
    async fn send_with_retry_stops_on_non_retryable_after_retryable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // First request: 500 (retryable), high priority, once
        Mock::given(method("POST"))
            .and(path("/test"))
            .respond_with(ResponseTemplate::new(500).set_body_string("error"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        // Fallback: 401 (non-retryable, default priority)
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

    // -- validate_sse_content_type tests --

    #[tokio::test]
    async fn validate_content_type_accepts_event_stream() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let resp = reqwest::get(format!("{}/ok", server.uri())).await.expect("request");
        assert!(validate_sse_content_type(&resp).is_ok());
    }

    #[tokio::test]
    async fn validate_content_type_accepts_event_stream_with_charset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("", "text/event-stream; charset=utf-8"),
            )
            .mount(&server)
            .await;

        let resp = reqwest::get(format!("{}/ok", server.uri())).await.expect("request");
        assert!(validate_sse_content_type(&resp).is_ok());
    }

    #[tokio::test]
    async fn validate_content_type_rejects_json() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bad"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("{}", "application/json"),
            )
            .mount(&server)
            .await;

        let resp = reqwest::get(format!("{}/bad", server.uri())).await.expect("request");
        let err = validate_sse_content_type(&resp);
        assert!(matches!(err, Err(ProviderError::SseParse(msg)) if msg.contains("application/json")));
    }

    #[tokio::test]
    async fn validate_content_type_tolerates_missing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/none"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let resp = reqwest::get(format!("{}/none", server.uri())).await.expect("request");
        assert!(validate_sse_content_type(&resp).is_ok());
    }
}
