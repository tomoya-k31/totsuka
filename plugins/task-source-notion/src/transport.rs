//! REST transport: the seam between the plugin's logic and the network.
//!
//! [`NotionClient`](crate::client::NotionClient) is generic over
//! [`NotionTransport`] so its fetch/update/publish/validate logic is exercised
//! in tests against a recorded-response fake, while production uses
//! [`ReqwestTransport`] (bearer auth, the pinned `Notion-Version` header,
//! a rate throttle, and capped exponential backoff, §5.3).

use std::future::Future;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::NotionError;

/// HTTP method for a Notion REST call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET` — read (databases, blocks, users).
    Get,
    /// `POST` — query a database.
    Post,
    /// `PATCH` — update a page or append blocks.
    Patch,
}

/// Connection settings for building a transport. Grouped so factories and
/// constructors take one argument rather than a long positional list.
#[derive(Debug, Clone, Copy)]
pub struct TransportSettings<'a> {
    /// REST base URL (no trailing slash), e.g. `https://api.notion.com/v1`.
    pub api_url: &'a str,
    /// Bearer token.
    pub token: &'a str,
    /// Pinned `Notion-Version` header value.
    pub api_version: &'a str,
    /// Max retry attempts for retryable failures.
    pub max_retries: u32,
    /// Client-side request cap (requests/second).
    pub rate_limit_rps: u32,
}

/// Sends a Notion REST request and returns the parsed JSON body.
pub trait NotionTransport: Send + Sync {
    /// Perform `method path` with an optional JSON `body`, returning the parsed
    /// response JSON.
    ///
    /// `path` is relative to the configured base URL (e.g.
    /// `/databases/{id}/query`). `idempotent` guards automatic retries: a
    /// timed-out/5xx request is only re-sent when replaying it is safe.
    /// **Every current caller passes `true`** — the one that could duplicate
    /// (appending page blocks) went with `result/publish` (#398). The flag
    /// stays because a future create-shaped call would need it, and finding
    /// that out from duplicated content is expensive.
    fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, NotionError>> + Send;
}

/// The production transport: reqwest against Notion's REST API.
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
    token: String,
    api_version: String,
    timeout: Duration,
    max_retries: u32,
    backoff_base: Duration,
    /// Minimum spacing between requests (the rate throttle).
    min_interval: Duration,
    /// The earliest instant the next request may start. Held across the pre-send
    /// wait so concurrent calls are serialized to `min_interval` spacing.
    next_allowed: Mutex<Option<Instant>>,
}

impl ReqwestTransport {
    /// A transport from connection `settings`.
    pub fn new(settings: TransportSettings<'_>) -> Self {
        // Guard against a zero rps (which would divide by zero): treat it as
        // "no throttle" via a zero interval. Ceil division so the effective rate
        // never *exceeds* the configured rps — plain `Duration` division
        // truncates (1s/3 → 333ms), which would allow a hair over the limit.
        let min_interval = if settings.rate_limit_rps == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(1_000_000_000u64.div_ceil(u64::from(settings.rate_limit_rps)))
        };
        Self {
            client: reqwest::Client::new(),
            base_url: settings.api_url.trim_end_matches('/').to_string(),
            token: settings.token.to_string(),
            api_version: settings.api_version.to_string(),
            timeout: Duration::from_secs(30),
            max_retries: settings.max_retries,
            backoff_base: Duration::from_millis(500),
            min_interval,
            next_allowed: Mutex::new(None),
        }
    }

    /// Wait until the throttle permits another request, then reserve the slot.
    async fn throttle(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let mut slot = self.next_allowed.lock().await;
        let now = Instant::now();
        let start = match *slot {
            Some(at) if at > now => {
                tokio::time::sleep(at - now).await;
                at
            }
            _ => now,
        };
        *slot = Some(start + self.min_interval);
    }

    /// One HTTP attempt, mapping transport/status failures to [`NotionError`].
    async fn attempt(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, NotionError> {
        self.throttle().await;
        let url = format!("{}{}", self.base_url, path);
        let mut req = match method {
            HttpMethod::Get => self.client.get(&url),
            HttpMethod::Post => self.client.post(&url),
            HttpMethod::Patch => self.client.patch(&url),
        }
        .bearer_auth(&self.token)
        .header("Notion-Version", &self.api_version)
        .timeout(self.timeout);
        if let Some(body) = body {
            req = req.json(body);
        }

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                NotionError::Timeout(self.timeout.as_secs())
            } else {
                NotionError::Transport(e.to_string())
            }
        })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(NotionError::Unauthorized);
        }
        let text = response
            .text()
            .await
            .map_err(|e| NotionError::Transport(e.to_string()))?;
        if !status.is_success() {
            let body: String = text.chars().take(500).collect();
            // 404 gets its own variant: it is the one status whose next action
            // ("is it shared with this token?") differs from "the call failed".
            //
            // But only when Notion itself said `object_not_found`. `api_url` is
            // configurable, so a 404 can equally come from a mistyped base URL
            // or a proxy in front of it — and telling *that* reader to check
            // sharing is the same wrong lead this variant exists to remove.
            let object_not_found = status == reqwest::StatusCode::NOT_FOUND
                && serde_json::from_str::<Value>(&text)
                    .ok()
                    .is_some_and(|v| v["code"].as_str() == Some("object_not_found"));
            return Err(if object_not_found {
                NotionError::ObjectNotFound(body)
            } else {
                NotionError::Http {
                    status: status.as_u16(),
                    body,
                }
            });
        }
        serde_json::from_str(&text).map_err(|e| NotionError::InvalidResponse(e.to_string()))
    }
}

impl NotionTransport for ReqwestTransport {
    async fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> Result<Value, NotionError> {
        let mut attempt = 0;
        loop {
            match self.attempt(method, path, body.as_ref()).await {
                Ok(value) => return Ok(value),
                // Only replay when it is safe to: a non-idempotent mutation
                // whose response was lost must surface the error, not re-run.
                Err(e) if idempotent && e.is_retryable() && attempt < self.max_retries => {
                    let factor = 2u32.saturating_pow(attempt);
                    let delay = self
                        .backoff_base
                        .saturating_mul(factor)
                        .min(Duration::from_secs(60));
                    tracing::warn!(attempt, error = %e, "notion call failed; retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve exactly one HTTP response with `status` and `body`. Returns the
    /// base URL to point a transport at, plus the server's handle — awaiting it
    /// keeps the task from outliving the test (nextest reports that as "leaky").
    async fn one_shot(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain the whole request before replying. Closing a socket with
            // bytes still queued lets the kernel send RST instead of FIN, which
            // can discard the response already written and surface as a
            // transport error rather than the status under test.
            //
            // This is hardening, not a fix for an observed failure: measured on
            // loopback, a single `read` already picked up the whole request for
            // every call here — including the POST below — and those tests
            // passed without this drain. It is here because the size at which
            // that stops holding is not a property of these tests, and the
            // failure it would produce looks like a flake rather than a bug.
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            let head_end = loop {
                let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    break req.len();
                }
                req.extend_from_slice(&buf[..n]);
                if let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let want = std::str::from_utf8(&req[..head_end])
                .ok()
                .and_then(|head| {
                    head.lines().find_map(|l| {
                        l.strip_prefix("content-length: ")
                            .or_else(|| l.strip_prefix("Content-Length: "))
                    })
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while req.len() < head_end + want {
                let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                    .await
                    .unwrap_or(0);
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, resp.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut sock).await;
        });
        (format!("http://{addr}"), server)
    }

    fn transport(api_url: &str) -> ReqwestTransport {
        ReqwestTransport::new(TransportSettings {
            api_url,
            token: "t",
            api_version: "2022-06-28",
            // No retries: a 5xx is retryable and the one-shot server answers once.
            max_retries: 0,
            // No throttle — this is a mapping test, not a pacing one.
            rate_limit_rps: 0,
        })
    }

    /// 404 is the one status whose next action differs from "the call failed",
    /// so it must not fall into the generic [`NotionError::Http`] bucket.
    #[tokio::test]
    async fn maps_404_to_object_not_found() {
        let (url, server) = one_shot(404, r#"{"code":"object_not_found"}"#).await;
        let err = transport(&url)
            .request(HttpMethod::Get, "/databases/x", None, true)
            .await
            .expect_err("404 is an error");
        match err {
            NotionError::ObjectNotFound(body) => {
                assert!(body.contains("object_not_found"), "{body}")
            }
            other => panic!("expected ObjectNotFound, got {other:?}"),
        }
        server.await.unwrap();
    }

    /// A 404 that is *not* Notion's `object_not_found` keeps the generic
    /// mapping: `api_url` is configurable, so this is what a mistyped base URL
    /// or a proxy produces, and the sharing guidance would be a wrong lead.
    #[tokio::test]
    async fn keeps_a_non_notion_404_generic() {
        let (url, server) = one_shot(404, "<html>not found</html>").await;
        let err = transport(&url)
            .request(HttpMethod::Get, "/databases/x", None, true)
            .await
            .expect_err("404 is an error");
        match err {
            NotionError::Http { status, .. } => assert_eq!(status, 404),
            other => panic!("expected Http, got {other:?}"),
        }
        server.await.unwrap();
    }

    /// The same mapping over a POST **with a body** — the plugin's main call is
    /// `POST /databases/{id}/query`, so pin the mapping for a request that
    /// carries one rather than only for bodyless GETs.
    #[tokio::test]
    async fn maps_404_on_a_request_with_a_body() {
        let (url, server) = one_shot(404, r#"{"code":"object_not_found"}"#).await;
        let err = transport(&url)
            .request(
                HttpMethod::Post,
                "/databases/x/query",
                Some(serde_json::json!({ "page_size": 100 })),
                true,
            )
            .await
            .expect_err("404 is an error");
        assert!(matches!(err, NotionError::ObjectNotFound(_)), "{err:?}");
        server.await.unwrap();
    }

    /// 401 stays its own variant — and, crucially, is *not* what an unshared
    /// resource produces.
    #[tokio::test]
    async fn maps_401_to_unauthorized() {
        let (url, server) = one_shot(401, r#"{"code":"unauthorized"}"#).await;
        let err = transport(&url)
            .request(HttpMethod::Get, "/users/me", None, true)
            .await
            .expect_err("401 is an error");
        assert!(matches!(err, NotionError::Unauthorized), "{err:?}");
        server.await.unwrap();
    }

    /// Every other failing status keeps the generic mapping, so adding the 404
    /// branch did not swallow the rest.
    #[tokio::test]
    async fn maps_other_failures_to_http() {
        let (url, server) = one_shot(500, r#"{"code":"internal_server_error"}"#).await;
        let err = transport(&url)
            .request(HttpMethod::Get, "/users/me", None, true)
            .await
            .expect_err("500 is an error");
        match err {
            NotionError::Http { status, .. } => assert_eq!(status, 500),
            other => panic!("expected Http, got {other:?}"),
        }
        server.await.unwrap();
    }
}
