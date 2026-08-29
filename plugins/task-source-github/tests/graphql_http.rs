//! [`ReqwestTransport`] behavior against a real (local) HTTP server: the
//! headers GitHub requires on the wire, the status → error-variant mapping,
//! and the retry discipline for idempotent vs. non-idempotent calls.
//!
//! **Why this file exists.** `tests/integration.rs` drives the plugin through a
//! fake transport that returns [`GithubError`] variants *directly*, so nothing
//! there exercises the real transport: delete the 401 branch, or the
//! `User-Agent` header GitHub rejects requests without, and that suite stays
//! green. This mirrors `task-source-slack`'s `tests/web_api_http.rs`, which
//! already covers the same ground for the Slack transport.
//!
//! The mock is a raw TCP loop serving canned HTTP/1.1 responses — no HTTP
//! server dependency, mirroring the workspace's no-new-deps test policy.
//!
//! Not covered: the timeout → [`GithubError::Timeout`] mapping. The 30s timeout
//! is hard-coded in `ReqwestTransport::new` with no knob to shorten it, so
//! pinning it would cost 30s of wall clock per run. Reaching it would need a
//! settings field, which is a production change, not a test one.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use task_source_github::error::GithubError;
use task_source_github::transport::{GithubTransport, ReqwestTransport};

/// One canned HTTP response: status line, extra headers, and a body.
struct CannedHttp {
    status: &'static str,
    headers: Vec<String>,
    body: String,
}

impl CannedHttp {
    fn ok(body: Value) -> Self {
        Self {
            status: "200 OK",
            headers: Vec::new(),
            body: body.to_string(),
        }
    }
    fn status(status: &'static str, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }
    /// A throttle that names its wait in `retry-after`.
    fn rate_limited(retry_after: &str) -> Self {
        Self {
            status: "429 Too Many Requests",
            headers: vec![format!("Retry-After: {retry_after}")],
            body: String::new(),
        }
    }
    /// A throttle with no `retry-after`, reporting a spent budget instead —
    /// GitHub's second case, and the one that arrives as a **403**.
    fn budget_spent(reset_epoch_secs: u64) -> Self {
        Self {
            status: "403 Forbidden",
            headers: vec![
                "X-RateLimit-Remaining: 0".into(),
                format!("X-RateLimit-Reset: {reset_epoch_secs}"),
            ],
            body: String::new(),
        }
    }
}

/// Serve `responses` in order, one per connection, recording each raw request.
/// Returns the base URL, the request log, and the guard that stops the server.
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
            // Read until the headers are complete, then drain the body by
            // Content-Length. Closing with bytes still queued can make the
            // kernel send RST instead of FIN and discard the response.
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
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
                        .find_map(|l| {
                            l.strip_prefix("content-length: ")
                                .or_else(|| l.strip_prefix("Content-Length: "))
                        })
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if tail.len() >= expected {
                        break text.into_owned();
                    }
                }
            };
            log.lock().unwrap().push(request);

            let response = format!(
                "HTTP/1.1 {}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    ReqwestTransport::new(base.to_string(), "t0ken".to_string(), max_retries)
        // Fast backoff so the 5xx retry tests do not pay production-scale
        // sleeps. The `retry-after` waits stay real — the header is whole
        // seconds — and the 5s budget is what the fail-fast tests lean on.
        .with_retry_timing(Duration::from_millis(10), Duration::from_secs(5))
}

fn query() -> Value {
    json!({ "query": "{ viewer { login } }" })
}

/// The wire contract GitHub imposes: bearer auth, a JSON body, and a
/// `User-Agent` — GitHub rejects GraphQL requests that omit the last one, and
/// nothing else in the suite would notice it going away.
#[tokio::test]
async fn sends_bearer_auth_a_user_agent_and_the_json_query() {
    let (base, log) =
        mock_server(vec![CannedHttp::ok(json!({ "data": { "viewer": null } }))]).await;

    transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect("200 succeeds");

    let requests = log.lock().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let req = requests[0].to_ascii_lowercase();
    assert!(req.starts_with("post "), "{}", requests[0]);
    assert!(
        req.contains("authorization: bearer t0ken"),
        "{}",
        requests[0]
    );
    assert!(req.contains("user-agent: totsuka"), "{}", requests[0]);
    assert!(
        req.contains("content-type: application/json"),
        "{}",
        requests[0]
    );
    assert!(requests[0].contains("viewer { login }"), "{}", requests[0]);
}

/// 401 is special-cased before the body is even read, so it must not fall into
/// the generic [`GithubError::Http`] bucket — its message is the one that names
/// the config key and scopes.
#[tokio::test]
async fn unauthorized_gets_its_own_variant() {
    let (base, _log) = mock_server(vec![CannedHttp::status("401 Unauthorized", "nope")]).await;

    let err = transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect_err("401 is an error");

    assert!(matches!(err, GithubError::Unauthorized), "{err:?}");
}

/// Every other failing status keeps the generic mapping, carrying the status
/// through so the operator sees which one it was.
#[tokio::test]
async fn other_failures_carry_their_status() {
    let (base, _log) =
        mock_server(vec![CannedHttp::status("422 Unprocessable", "bad query")]).await;

    let err = transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect_err("422 is an error");

    match err {
        GithubError::Http { status, body } => {
            assert_eq!(status, 422);
            assert_eq!(body, "bad query");
        }
        other => panic!("expected Http, got {other:?}"),
    }
}

/// The body is truncated so a huge error page cannot flood the log — and it is
/// truncated **by codepoint, not by byte**.
///
/// The body is deliberately multibyte. With ASCII the two are
/// indistinguishable, so an ASCII fixture would pass just as happily against
/// `text[..500]` — which panics on a non-boundary index, *inside error
/// handling*, destroying the original HTTP failure. This repo's issue titles
/// are Japanese, so that body is the realistic one.
#[tokio::test]
async fn a_long_error_body_is_truncated_on_a_codepoint_boundary() {
    let long = "あ".repeat(2000);
    let (base, _log) =
        mock_server(vec![CannedHttp::status("500 Internal Server Error", &long)]).await;

    let err = transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect_err("500 is an error");

    match err {
        GithubError::Http { body, .. } => assert_eq!(body.chars().count(), 500),
        other => panic!("expected Http, got {other:?}"),
    }
}

/// A retryable failure is replayed when the call is safe to replay.
#[tokio::test]
async fn a_server_error_is_retried_for_an_idempotent_call() {
    let (base, log) = mock_server(vec![
        CannedHttp::status("500 Internal Server Error", "oops"),
        CannedHttp::ok(json!({ "data": { "ok": true } })),
    ])
    .await;

    let value = transport(&base, 1)
        .post_graphql(query(), true)
        .await
        .expect("the retry succeeds");

    assert_eq!(value["data"]["ok"], true);
    assert_eq!(log.lock().unwrap().len(), 2, "expected one retry");
}

/// **The load-bearing one.** A mutation whose response was lost must surface the
/// error rather than run twice — replaying it could duplicate a side effect.
#[tokio::test]
async fn a_server_error_is_not_retried_for_a_non_idempotent_call() {
    let (base, log) = mock_server(vec![CannedHttp::status(
        "500 Internal Server Error",
        "oops",
    )])
    .await;

    let err = transport(&base, 1)
        .post_graphql(query(), false)
        .await
        .expect_err("a non-idempotent call must not be replayed");

    assert!(
        matches!(err, GithubError::Http { status: 500, .. }),
        "{err:?}"
    );
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "a lost mutation response must surface, not re-run"
    );
}

/// **A rejected token must not be replayed.** `is_retryable` answers `false`
/// for everything outside 429/5xx/transport, and nothing above pins that: every
/// other failure test runs with a retry budget of zero, which hides the
/// vocabulary entirely. Without this, flipping `is_retryable`'s fallthrough to
/// `true` stays green — and in production an expired token would be replayed on
/// every `poll_loop` tick, which is exactly what GitHub rate-limits.
#[tokio::test]
async fn unauthorized_is_not_retried_even_with_a_budget() {
    let (base, log) = mock_server(vec![CannedHttp::status("401 Unauthorized", "nope")]).await;

    let err = transport(&base, 1)
        .post_graphql(query(), true)
        .await
        .expect_err("401 is an error");

    assert!(matches!(err, GithubError::Unauthorized), "{err:?}");
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "a rejected token is not a transient failure"
    );
}

/// The same for a 4xx that is not 429: retryable is 429 and 5xx, not "any
/// failing status". A budget is given so the absence of a retry is a result,
/// not an artefact of `max_retries = 0`.
#[tokio::test]
async fn a_client_error_is_not_retried_even_with_a_budget() {
    let (base, log) = mock_server(vec![CannedHttp::status("422 Unprocessable", "bad query")]).await;

    let err = transport(&base, 1)
        .post_graphql(query(), true)
        .await
        .expect_err("422 is an error");

    assert!(
        matches!(err, GithubError::Http { status: 422, .. }),
        "{err:?}"
    );
    assert_eq!(log.lock().unwrap().len(), 1, "422 is not transient");
}

/// **`retry-after` is honoured exactly, and a throttle may be replayed even
/// when the call is not idempotent.** A throttled request never ran, so
/// replaying it cannot duplicate a side effect — unlike a lost 5xx.
#[tokio::test]
async fn a_throttle_waits_the_requested_time_and_replays_a_non_idempotent_call() {
    let (base, log) = mock_server(vec![
        CannedHttp::rate_limited("1"),
        CannedHttp::ok(json!({ "data": { "ok": true } })),
    ])
    .await;

    let started = Instant::now();
    let value = transport(&base, 3)
        .post_graphql(query(), false)
        .await
        .expect("the retry succeeds");

    assert_eq!(value["data"]["ok"], true);
    assert_eq!(log.lock().unwrap().len(), 2, "expected one retry");
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "must wait what `retry-after` asked for, waited only {:?}",
        started.elapsed()
    );
}

/// A wait longer than the budget surfaces the throttle immediately instead of
/// sleeping. One `poll_loop` tick parked for two minutes is indistinguishable
/// from a wedged plugin.
#[tokio::test]
async fn a_wait_beyond_the_budget_fails_fast_instead_of_sleeping() {
    let (base, log) = mock_server(vec![CannedHttp::rate_limited("120")]).await;

    let started = Instant::now();
    let err = transport(&base, 3)
        .post_graphql(query(), true)
        .await
        .expect_err("the budget is spent before the wait");

    assert!(
        matches!(
            err,
            GithubError::RateLimited {
                retry_after_secs: 120
            }
        ),
        "{err:?}"
    );
    assert_eq!(log.lock().unwrap().len(), 1, "no premature retry");
    assert!(started.elapsed() < Duration::from_secs(2), "must not sleep");
}

/// An HTTP-date `retry-after` is valid per RFC 9110 but not a number. Falling
/// back to a small value would turn it into a hammer loop, so the fallback is a
/// minute — which exceeds the test budget, hence one request and no sleep.
#[tokio::test]
async fn an_unparseable_retry_after_falls_back_conservatively() {
    let (base, log) = mock_server(vec![CannedHttp::rate_limited(
        "Wed, 21 Oct 2026 07:28:00 GMT",
    )])
    .await;

    let started = Instant::now();
    let err = transport(&base, 3)
        .post_graphql(query(), true)
        .await
        .expect_err("the fallback exceeds the budget");

    assert!(
        matches!(
            err,
            GithubError::RateLimited {
                retry_after_secs: 60
            }
        ),
        "{err:?}"
    );
    assert_eq!(log.lock().unwrap().len(), 1);
    assert!(started.elapsed() < Duration::from_secs(2), "must not sleep");
}

/// GitHub's second throttle shape: **a 403** with no `retry-after`, reporting a
/// spent budget through `x-ratelimit-remaining: 0` and an epoch-seconds
/// `x-ratelimit-reset`. Treating the status alone as the signal would miss it
/// entirely and fall into the generic `Http` bucket.
#[tokio::test]
async fn a_spent_budget_on_a_403_is_a_throttle() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (base, log) = mock_server(vec![CannedHttp::budget_spent(now + 300)]).await;

    let err = transport(&base, 3)
        .post_graphql(query(), true)
        .await
        .expect_err("300s exceeds the budget");

    match err {
        // ~300s, computed from the reset instant rather than taken literally.
        GithubError::RateLimited { retry_after_secs } => {
            assert!(
                (295..=300).contains(&retry_after_secs),
                "{retry_after_secs}"
            )
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    assert_eq!(log.lock().unwrap().len(), 1);
}

/// **A bare 403 is a permission error, not a throttle.** Retrying it would burn
/// the budget and bury the real cause, so the rate-limit headers — not the
/// status — are what decide.
#[tokio::test]
async fn a_403_without_rate_limit_headers_is_not_retried() {
    let (base, log) = mock_server(vec![CannedHttp::status(
        "403 Forbidden",
        "insufficient scopes",
    )])
    .await;

    let err = transport(&base, 3)
        .post_graphql(query(), true)
        .await
        .expect_err("403 is an error");

    assert!(
        matches!(err, GithubError::Http { status: 403, .. }),
        "{err:?}"
    );
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "a permission error is not transient"
    );
}

/// Exhausting the budget surfaces the last error, not a success or a panic.
#[tokio::test]
async fn retries_exhaust_into_the_last_error() {
    let (base, log) = mock_server(vec![
        CannedHttp::status("500 Internal Server Error", "first"),
        CannedHttp::status("503 Service Unavailable", "last"),
    ])
    .await;

    let err = transport(&base, 1)
        .post_graphql(query(), true)
        .await
        .expect_err("both attempts fail");

    match err {
        GithubError::Http { status, body } => {
            assert_eq!(status, 503);
            assert_eq!(body, "last");
        }
        other => panic!("expected Http, got {other:?}"),
    }
    assert_eq!(log.lock().unwrap().len(), 2);
}

/// The transport hands back the **whole** envelope, `errors` included — reading
/// them is the client's job, and a transport that swallowed them would hide
/// every GraphQL-level failure.
#[tokio::test]
async fn a_200_with_graphql_errors_is_passed_through_intact() {
    let envelope = json!({
        "data": null,
        "errors": [{ "message": "Could not resolve to a ProjectV2" }],
    });
    let (base, _log) = mock_server(vec![CannedHttp::ok(envelope.clone())]).await;

    let value = transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect("a 200 is not a transport error");

    assert_eq!(value, envelope);
}

/// A 200 that is not JSON is a response problem, not a transport one.
#[tokio::test]
async fn a_non_json_success_body_is_an_invalid_response() {
    let (base, _log) = mock_server(vec![CannedHttp::status("200 OK", "<html>hi</html>")]).await;

    let err = transport(&base, 0)
        .post_graphql(query(), true)
        .await
        .expect_err("html is not a GraphQL envelope");

    assert!(matches!(err, GithubError::InvalidResponse(_)), "{err:?}");
}
