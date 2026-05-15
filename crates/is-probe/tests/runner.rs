//! Integration tests for the streaming probe runner.
//!
//! These tests drive `runner::run` against a mock HTTP server
//! (`wiremock`) that emulates an OpenAI-compatible streamed
//! response. Network I/O is real (loopback), but the engine is
//! a controlled fake, so test outcomes are deterministic.

use is_probe::config::ProbeConfig;
use is_probe::error::ProbeError;
use is_probe::runner::run;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds a probe config pointing at a mock server.
fn config_for(server: &MockServer) -> ProbeConfig {
    ProbeConfig::new(server.uri(), "test-model", "hello", 32)
}

/// A canonical chunk body the way an OpenAI-compatible engine
/// would emit it: each token in its own `data:` line, terminated
/// by `data: [DONE]`. Returns the raw bytes ready to be served.
fn sse_body(tokens: &[&str], finish_reason: &str) -> String {
    let mut out = String::new();
    for tok in tokens {
        out.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{tok}\"}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    out.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{finish_reason}\"}}]}}\n\n"
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

#[tokio::test]
async fn run_records_one_arrival_per_token() {
    let server = MockServer::start().await;

    let body = sse_body(&["Hello", " ", "world"], "stop");
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let cfg = config_for(&server);
    let timing = run(&cfg).await.expect("probe should succeed");

    assert_eq!(timing.token_count(), 3);
    assert_eq!(timing.tokens[0].index, 0);
    assert_eq!(timing.tokens[1].index, 1);
    assert_eq!(timing.tokens[2].index, 2);
}

#[tokio::test]
async fn arrival_timestamps_are_monotonically_increasing() {
    let server = MockServer::start().await;

    let body = sse_body(&["one", "two", "three", "four"], "stop");
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let timing = run(&config_for(&server))
        .await
        .expect("probe should succeed");

    // elapsed_ns is monotonically non-decreasing across tokens.
    for pair in timing.tokens.windows(2) {
        assert!(
            pair[1].elapsed_ns >= pair[0].elapsed_ns,
            "elapsed_ns must not go backwards: {} then {}",
            pair[0].elapsed_ns,
            pair[1].elapsed_ns,
        );
    }

    // total_ns covers at least up to the last token.
    let last = timing.tokens.last().unwrap();
    assert!(timing.total_ns >= last.elapsed_ns);
}

#[tokio::test]
async fn http_error_status_is_surfaced() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let err = run(&config_for(&server))
        .await
        .expect_err("probe should fail");
    match err {
        ProbeError::HttpStatus { status, body } => {
            assert_eq!(status, 500);
            assert!(body.contains("internal error"));
        }
        other => panic!("expected HttpStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_json_in_a_chunk_is_reported() {
    let server = MockServer::start().await;

    // Valid SSE framing but the JSON payload is broken.
    let body = "data: {not valid json\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let err = run(&config_for(&server))
        .await
        .expect_err("probe should fail");
    assert!(
        matches!(err, ProbeError::MalformedChunk { .. }),
        "expected MalformedChunk, got {err:?}"
    );
}

#[tokio::test]
async fn stream_without_done_marker_is_truncated() {
    let server = MockServer::start().await;

    // Two valid tokens, but the stream ends without [DONE] and
    // without a finish_reason.
    let body = "\
data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" there\"},\"finish_reason\":null}]}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let err = run(&config_for(&server))
        .await
        .expect_err("probe should fail");
    match err {
        ProbeError::StreamTruncated { tokens_received } => {
            assert_eq!(tokens_received, 2);
        }
        other => panic!("expected StreamTruncated, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_intermediate_chunks_are_ignored() {
    let server = MockServer::start().await;

    // A real engine sometimes emits chunks with empty delta and no
    // finish reason as keepalives. The probe must not count them.
    let body = "\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"real\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body.as_bytes().to_vec(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let timing = run(&config_for(&server))
        .await
        .expect("probe should succeed");
    assert_eq!(timing.token_count(), 1);
}
