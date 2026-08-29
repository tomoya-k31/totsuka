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

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use task_source_github::error::GithubError;
use task_source_github::transport::{GithubTransport, ReqwestTransport};

/// One canned HTTP response: status line and a body.
struct CannedHttp {
    status: &'static str,
    body: String,
}

impl CannedHttp {
    fn ok(body: Value) -> Self {
        Self {
            status: "200 OK",
            body: body.to_string(),
        }
    }
    fn status(status: &'static str, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

/// Serve `responses` in order, one per connection, recording each raw request.
/// Returns the base URL and the request log.
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
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                canned.status,
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

/// The body is truncated so a huge error page cannot flood the log.
#[tokio::test]
async fn a_long_error_body_is_truncated() {
    let long = "x".repeat(2000);
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
    let (base, log) = mock_server(vec![
        CannedHttp::status("500 Internal Server Error", "oops"),
        CannedHttp::ok(json!({ "data": { "ok": true } })),
    ])
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
        "the second canned response must be untouched"
    );
}

/// Rate limiting is retryable too, on the same gate.
#[tokio::test]
async fn a_rate_limit_is_retried_for_an_idempotent_call() {
    let (base, log) = mock_server(vec![
        CannedHttp::status("429 Too Many Requests", ""),
        CannedHttp::ok(json!({ "data": { "ok": true } })),
    ])
    .await;

    transport(&base, 1)
        .post_graphql(query(), true)
        .await
        .expect("the retry succeeds");

    assert_eq!(log.lock().unwrap().len(), 2, "expected one retry");
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
