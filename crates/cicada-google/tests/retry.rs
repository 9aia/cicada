//! `RetryHttp` unit tests — backoff, jitter, and which results are
//! retryable — with an instant recording sleeper.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use cicada_core::domain::error::CicadaError;
use cicada_core::ports::{Http, HttpMethod, HttpRequest, HttpResponse};
use cicada_google::{FixedJitter, RecordingSleeper, RetryHttp, RetryPolicy};
use cicada_testkit::FixtureHttp;

fn get(url: &str) -> HttpRequest {
    HttpRequest::new(HttpMethod::Get, url)
}

fn ok() -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![],
        body: b"{}".to_vec(),
    }
}

fn status(code: u16) -> HttpResponse {
    HttpResponse {
        status: code,
        headers: vec![],
        body: vec![],
    }
}

fn fixture_sleeper(http: &FixtureHttp) -> (RetryHttp<FixtureHttp>, Arc<RecordingSleeper>) {
    let sleeper = Arc::new(RecordingSleeper::default());
    let retrying = RetryHttp::new(http.clone())
        .with_sleeper(sleeper.clone())
        .with_jitter(Arc::new(FixedJitter(1.0)));
    (retrying, sleeper)
}

#[tokio::test]
async fn success_passes_through() {
    let http = FixtureHttp::new();
    http.expect(HttpMethod::Get, "/x", ok());
    let (client, sleeper) = fixture_sleeper(&http);
    let resp = client.send(&get("https://example.test/x")).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(http.requests().len(), 1);
    assert!(sleeper.slept.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retryable_status_retries_once() {
    let http = FixtureHttp::new();
    http.expect(HttpMethod::Get, "/x", status(503));
    http.expect(HttpMethod::Get, "/x", ok());
    let (client, sleeper) = fixture_sleeper(&http);
    let resp = client.send(&get("https://example.test/x")).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(http.requests().len(), 2);
    // 250ms * 2^0 * jitter 1.0
    assert_eq!(
        sleeper.slept.lock().unwrap().as_slice(),
        &[Duration::from_millis(250)]
    );
}

#[tokio::test]
async fn transport_transient_retries() {
    let http = FixtureHttp::new();
    http.expect_fn(
        |r| r.url.contains("/x"),
        Err(CicadaError::transient("connect refused")),
    );
    http.expect_fn(|r| r.url.contains("/x"), Ok(ok()));
    let (client, _sleeper) = fixture_sleeper(&http);
    let resp = client.send(&get("https://example.test/x")).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(http.requests().len(), 2);
}

#[tokio::test]
async fn retry_after_header_extends_delay() {
    let http = FixtureHttp::new();
    http.expect(
        HttpMethod::Get,
        "/x",
        HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "3".into())],
            body: vec![],
        },
    );
    http.expect(HttpMethod::Get, "/x", ok());
    let (client, sleeper) = fixture_sleeper(&http);
    client.send(&get("https://example.test/x")).await.unwrap();
    assert_eq!(
        sleeper.slept.lock().unwrap().as_slice(),
        &[Duration::from_secs(3)]
    );
}

#[tokio::test]
async fn jitter_scales_backoff() {
    let http = FixtureHttp::new();
    http.expect(HttpMethod::Get, "/x", status(500));
    http.expect(HttpMethod::Get, "/x", status(500));
    http.expect(HttpMethod::Get, "/x", ok());
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = RetryHttp::new(http.clone())
        .with_sleeper(sleeper.clone())
        .with_jitter(Arc::new(FixedJitter(0.5)))
        .with_policy(RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        });
    client.send(&get("https://example.test/x")).await.unwrap();
    // 1000ms * 0.5, then 2000ms * 0.5.
    assert_eq!(
        sleeper.slept.lock().unwrap().as_slice(),
        &[Duration::from_millis(500), Duration::from_secs(1)]
    );
}

#[tokio::test]
async fn exhausted_retries_return_last_response() {
    let http = FixtureHttp::new();
    for _ in 0..3 {
        http.expect(HttpMethod::Get, "/x", status(500));
    }
    let (client, _sleeper) = fixture_sleeper(&http);
    let client = client.with_policy(RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    });
    let resp = client.send(&get("https://example.test/x")).await.unwrap();
    // Exhaustion hands the last response to the caller for
    // classification (Retry-After survives in its headers).
    assert_eq!(resp.status, 500);
    assert_eq!(http.requests().len(), 3);
}

#[tokio::test]
async fn non_retryable_status_is_not_retried() {
    let http = FixtureHttp::new();
    http.expect(HttpMethod::Get, "/x", status(404));
    let (client, sleeper) = fixture_sleeper(&http);
    let resp = client.send(&get("https://example.test/x")).await.unwrap();
    assert_eq!(resp.status, 404);
    assert_eq!(http.requests().len(), 1);
    assert!(sleeper.slept.lock().unwrap().is_empty());
}

#[tokio::test]
async fn non_retryable_error_is_not_retried() {
    let http = FixtureHttp::new();
    http.expect_fn(
        |r| r.url.contains("/x"),
        Err(CicadaError::auth("bad token")),
    );
    let (client, _sleeper) = fixture_sleeper(&http);
    let err = client
        .send(&get("https://example.test/x"))
        .await
        .unwrap_err();
    assert!(matches!(err, CicadaError::Auth(_)));
    assert_eq!(http.requests().len(), 1);
}
