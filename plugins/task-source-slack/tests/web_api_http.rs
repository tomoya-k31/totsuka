//! [`ReqwestTransport`] behavior against a real (local) HTTP server: form
//! encoding on the wire, bearer auth per token kind, the 429 `Retry-After`
//! path, and the retry discipline for idempotent vs. non-idempotent calls.
//!
//! The mock is a raw TCP loop serving canned HTTP/1.1 responses — no HTTP
//! server dependency, mirroring the workspace's no-new-deps test policy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use task_source_slack::error::SlackError;
use task_source_slack::transport::{
    ReqwestTransport, SlackTransport, TokenKind, TransportSettings,
};

/// One canned HTTP response: status line extras and a JSON body.
struct CannedHttp {
    status: &'static str,
    headers: Vec<String>,
    body: String,
}

impl CannedHttp {
    fn ok(body: Value) -> Self {
        Self {
            status: "200 OK",
            headers: vec!["Content-Type: application/json".into()],
            body: body.to_string(),
        }
    }
    fn rate_limited(retry_after_secs: u64) -> Self {
        Self::rate_limited_header(&retry_after_secs.to_string())
    }
    fn rate_limited_header(retry_after: &str) -> Self {
        Self {
            status: "429 Too Many Requests",
            headers: vec![format!("Retry-After: {retry_after}")],
            body: String::new(),
        }
    }
    fn server_error() -> Self {
        Self {
            status: "500 Internal Server Error",
            headers: Vec::new(),
            body: "oops".into(),
        }
    }
}

/// Serve `responses` in order, one per connection, recording each raw request
/// (start line + headers + body). Returns the base URL and the request log.
async fn mock_server(responses: Vec<CannedHttp>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();

    tokio::spawn(async move {
        for canned in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the headers are complete, then drain the body by
            // Content-Length (requests here are small; one extra read is fine).
            let request = loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break String::from_utf8_lossy(&raw).into_owned();
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, tail)) = text.split_once("\r\n\r\n") {
                    let expected: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .or_else(|| {
                            head.lines()
                                .find_map(|l| l.strip_prefix("Content-Length: "))
                        })
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    if tail.len() >= expected {
                        break text.into_owned();
                    }
                }
            };
            log.lock().unwrap().push(request);

            let response = format!(
                "HTTP/1.1 {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                canned.status,
                canned
                    .headers
                    .iter()
                    .map(|h| format!("{h}\r\n"))
                    .collect::<String>(),
                canned.body.len(),
                canned.body,
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    (base, requests)
}

fn transport(base: &str, max_retries: u32) -> ReqwestTransport {
    transport_with_bot(base, max_retries, None)
}

fn transport_with_bot(base: &str, max_retries: u32, bot_token: Option<&str>) -> ReqwestTransport {
    ReqwestTransport::new(TransportSettings {
        api_url: base,
        app_token: "xapp-1-A1-app",
        user_token: "xoxp-user",
        bot_token,
        max_retries,
    })
    // Fast backoff so retry tests do not pay production-scale sleeps (the
    // Retry-After waits are still real: the header is whole seconds).
    .with_retry_timing(Duration::from_millis(10), Duration::from_secs(5))
}

#[tokio::test]
async fn arguments_travel_form_encoded_with_bearer_auth() {
    let (base, requests) = mock_server(vec![CannedHttp::ok(json!({ "ok": true }))]).await;

    transport(&base, 0)
        .call(
            TokenKind::User,
            "conversations.replies",
            Some(json!({ "channel": "C1", "limit": 4 })),
            true,
        )
        .await
        .unwrap();

    let raw = requests.lock().unwrap()[0].clone();
    assert!(
        raw.starts_with("POST /conversations.replies HTTP/1.1"),
        "{raw}"
    );
    assert!(raw.contains("authorization: Bearer xoxp-user"), "{raw}");
    assert!(raw.contains("application/x-www-form-urlencoded"), "{raw}");
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    assert!(body.contains("channel=C1"), "{body}");
    assert!(body.contains("limit=4"), "{body}");
}

#[tokio::test]
async fn app_token_authenticates_app_calls() {
    let (base, requests) = mock_server(vec![CannedHttp::ok(
        json!({ "ok": true, "url": "wss://x" }),
    )])
    .await;

    transport(&base, 0)
        .call(TokenKind::App, "apps.connections.open", None, true)
        .await
        .unwrap();

    let raw = requests.lock().unwrap()[0].clone();
    assert!(raw.contains("authorization: Bearer xapp-1-A1-app"), "{raw}");
}

#[tokio::test]
async fn bot_token_authenticates_bot_calls() {
    let (base, requests) =
        mock_server(vec![CannedHttp::ok(json!({ "ok": true, "ts": "1.0" }))]).await;

    transport_with_bot(&base, 0, Some("xoxb-bot"))
        .call(
            TokenKind::Bot,
            "chat.postMessage",
            Some(json!({ "channel": "D_BOT", "text": "🔔" })),
            false,
        )
        .await
        .unwrap();

    let raw = requests.lock().unwrap()[0].clone();
    assert!(raw.contains("authorization: Bearer xoxb-bot"), "{raw}");
}

#[tokio::test]
async fn bot_call_without_a_configured_token_fails_as_a_plugin_bug() {
    // No HTTP request may leave the process: the failure is local
    // (InvalidRequest), not a Slack error blamed on the network.
    let (base, requests) = mock_server(vec![]).await;

    let err = transport(&base, 3)
        .call(TokenKind::Bot, "chat.postMessage", None, false)
        .await
        .unwrap_err();
    assert!(matches!(err, SlackError::InvalidRequest(_)), "{err}");
    assert!(err.to_string().contains("bot_token"), "{err}");
    assert!(requests.lock().unwrap().is_empty(), "no request sent");
}

#[tokio::test]
async fn rate_limited_call_waits_retry_after_and_retries_even_when_not_idempotent() {
    let (base, requests) = mock_server(vec![
        CannedHttp::rate_limited(1),
        CannedHttp::ok(json!({ "ok": true, "ts": "9.0" })),
    ])
    .await;

    let started = std::time::Instant::now();
    // Non-idempotent (a post): a 429 was rejected, so replaying is safe.
    let response = transport(&base, 3)
        .call(
            TokenKind::User,
            "chat.postMessage",
            Some(json!({ "channel": "C1", "text": "x" })),
            false,
        )
        .await
        .unwrap();
    assert_eq!(response["ts"], "9.0");
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "must honor Retry-After, waited only {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn server_error_is_not_retried_for_a_non_idempotent_call() {
    let (base, requests) = mock_server(vec![CannedHttp::server_error()]).await;

    let err = transport(&base, 3)
        .call(
            TokenKind::User,
            "chat.postMessage",
            Some(json!({ "channel": "C1", "text": "x" })),
            false,
        )
        .await
        .unwrap_err();
    // A 5xx may have applied the write; the error surfaces instead of re-posting.
    assert!(matches!(err, SlackError::Http { status: 500, .. }), "{err}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn server_error_is_retried_for_an_idempotent_call() {
    let (base, requests) = mock_server(vec![
        CannedHttp::server_error(),
        CannedHttp::ok(json!({ "ok": true, "messages": [] })),
    ])
    .await;

    let response = transport(&base, 3)
        .call(
            TokenKind::User,
            "conversations.replies",
            Some(json!({ "channel": "C1", "ts": "1.0" })),
            true,
        )
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn retries_exhaust_into_the_last_error() {
    let (base, requests) = mock_server(vec![
        CannedHttp::rate_limited(1),
        CannedHttp::rate_limited(1),
    ])
    .await;

    let err = transport(&base, 1)
        .call(TokenKind::User, "auth.test", None, true)
        .await
        .unwrap_err();
    assert!(matches!(err, SlackError::RateLimited { .. }), "{err}");
    assert_eq!(requests.lock().unwrap().len(), 2, "initial + 1 retry");
}

#[tokio::test]
async fn retry_after_beyond_the_budget_fails_fast_instead_of_sleeping() {
    // Retry-After: 120 exceeds the 5s test budget: the call must surface the
    // rate limit immediately (one request, no sleep), not appear to hang —
    // initialize's TokenGuard runs through this path.
    let (base, requests) = mock_server(vec![CannedHttp::rate_limited(120)]).await;

    let started = std::time::Instant::now();
    let err = transport(&base, 3)
        .call(TokenKind::User, "auth.test", None, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SlackError::RateLimited {
                retry_after_secs: 120,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1, "no premature retry");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn unparseable_retry_after_falls_back_conservatively() {
    // An HTTP-date Retry-After (valid per RFC 9110) must not collapse into a
    // ~1s hammer loop: the fallback is 30s, which exceeds the 5s test budget,
    // so the call stops after one request.
    let (base, requests) = mock_server(vec![CannedHttp::rate_limited_header(
        "Tue, 15 Jul 2026 10:00:00 GMT",
    )])
    .await;

    let err = transport(&base, 3)
        .call(TokenKind::User, "auth.test", None, true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SlackError::RateLimited {
                retry_after_secs: 30,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1, "no hammering");
}

#[tokio::test]
async fn post_url_maps_429_to_rate_limited() {
    // The response_url channel classifies a 429 like the Web API path does
    // (RateLimited, not a generic Http error) — it is just never auto-retried.
    let (base, requests) = mock_server(vec![CannedHttp::rate_limited(7)]).await;

    let err = transport(&base, 3)
        .post_url(&format!("{base}/response-url"), json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SlackError::RateLimited {
                retry_after_secs: 7,
                ..
            }
        ),
        "{err}"
    );
    // The URL is a capability secret: it must not leak into the error text.
    assert!(!err.to_string().contains(&base), "{err}");
    assert_eq!(requests.lock().unwrap().len(), 1, "never auto-retried");
}

#[tokio::test]
async fn post_url_sends_json_and_maps_failure_status() {
    let (base, requests) = mock_server(vec![
        CannedHttp::ok(json!({ "ok": true })),
        CannedHttp::server_error(),
    ])
    .await;

    let t = transport(&base, 3);
    t.post_url(
        &format!("{base}/response-url"),
        json!({ "replace_original": true }),
    )
    .await
    .unwrap();
    let raw = requests.lock().unwrap()[0].clone();
    assert!(raw.starts_with("POST /response-url"), "{raw}");
    assert!(raw.contains("application/json"), "{raw}");
    assert!(raw.contains(r#""replace_original":true"#), "{raw}");

    // Failures surface (the URL is 5-uses-only; no auto-retry).
    let err = t
        .post_url(&format!("{base}/response-url"), json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, SlackError::Http { status: 500, .. }), "{err}");
    assert_eq!(requests.lock().unwrap().len(), 2);
}

/// A listener that never answers. Nothing accepts it: the kernel completes the
/// handshake from the backlog, so the client connects, sends, and then waits —
/// which is what a timeout needs, and what dropping the socket would *not*
/// produce (that is a reset, i.e. `Transport`).
///
/// The listener is handed back so the test owns it; nothing is spawned.
async fn silent_server() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (base, listener)
}

/// How long the timeout tests let a request hang.
const TEST_TIMEOUT: Duration = Duration::from_millis(500);

/// Halfway between one attempt (~500ms) and two (~1000ms).
const RETRY_BOUNDARY: Duration = Duration::from_millis(800);

/// A request that never gets an answer becomes [`SlackError::Timeout`], not a
/// transport error — different next actions, and nothing pinned which one this
/// path produces until `with_timeout` existed.
#[tokio::test]
async fn a_request_that_never_answers_becomes_a_timeout() {
    let (base, _listener) = silent_server().await;

    let started = std::time::Instant::now();
    let err = transport(&base, 0)
        .with_timeout(TEST_TIMEOUT)
        .call(TokenKind::User, "auth.test", None, true)
        .await
        .unwrap_err();

    assert!(matches!(err, SlackError::Timeout(_)), "{err}");
    assert!(started.elapsed() >= TEST_TIMEOUT, "{:?}", started.elapsed());
}

/// **A timed-out post must not be replayed.** Unlike a 429 — which is
/// `is_rejected`, so provably never ran — a timeout may have applied the write
/// and only lost the answer. This is the one place the two retryable classes
/// must behave differently.
#[tokio::test]
async fn a_timeout_is_not_retried_for_a_non_idempotent_call() {
    let (base, _listener) = silent_server().await;

    let started = std::time::Instant::now();
    let err = transport(&base, 3)
        .with_timeout(TEST_TIMEOUT)
        .call(
            TokenKind::User,
            "chat.postMessage",
            Some(json!({ "channel": "C1", "text": "x" })),
            false,
        )
        .await
        .unwrap_err();

    assert!(matches!(err, SlackError::Timeout(_)), "{err}");
    assert!(
        started.elapsed() < RETRY_BOUNDARY,
        "must stop after one attempt, took {:?}",
        started.elapsed()
    );
}

/// An idempotent call is replayed, and still surfaces the timeout.
#[tokio::test]
async fn a_timeout_is_retried_for_an_idempotent_call() {
    let (base, _listener) = silent_server().await;

    let started = std::time::Instant::now();
    let err = transport(&base, 1)
        .with_timeout(TEST_TIMEOUT)
        .call(TokenKind::User, "auth.test", None, true)
        .await
        .unwrap_err();

    assert!(matches!(err, SlackError::Timeout(_)), "{err}");
    assert!(
        started.elapsed() >= RETRY_BOUNDARY,
        "one retry means two waits, took only {:?}",
        started.elapsed()
    );
}
