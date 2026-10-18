//! [`HttpTransport`]: one authenticated JSON POST — the seam every backend
//! sends through and every test fakes.

use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::Value;

use crate::ClassifyError;

/// An API key. `Debug` never prints it.
#[derive(Clone, Default)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wrap a resolved key. An empty key is legal: a keyless local gateway.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself, for the `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// One POST: `body` as JSON to `url`, bearer-authenticated with `api_key`.
///
/// The key travels with the request rather than living in the transport, so
/// one transport (one connection pool) serves whichever key the caller holds
/// — the Slack plugin learns its key only at `initialize`.
#[derive(Debug, Clone, Copy)]
pub struct HttpRequest<'a> {
    /// The full endpoint URL.
    pub url: &'a str,
    /// Sent as `Authorization: Bearer …`.
    pub api_key: &'a ApiKey,
    /// The JSON body.
    pub body: &'a Value,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// Sends one [`HttpRequest`] and returns the parsed 2xx response body.
///
/// Failures map onto [`ClassifyError`]: `Transport`/`Timeout` when no
/// response arrived, `Status` for a non-2xx ([`ClassifyError::status`]) even
/// if its body could not be read, `InvalidResponse` for a 2xx whose body is
/// unreadable or not JSON. No retries here — that is
/// [`RetryPolicy`](crate::RetryPolicy)'s job.
pub trait HttpTransport: Send + Sync {
    /// POST and parse.
    fn post_json(
        &self,
        request: HttpRequest<'_>,
    ) -> impl Future<Output = Result<Value, ClassifyError>> + Send;

    /// Drop pooled connections (F-111). A no-op for transports without any.
    fn reset_connections(&self) {}
}

impl<T: HttpTransport + ?Sized> HttpTransport for &T {
    fn post_json(
        &self,
        request: HttpRequest<'_>,
    ) -> impl Future<Output = Result<Value, ClassifyError>> + Send {
        (**self).post_json(request)
    }

    fn reset_connections(&self) {
        (**self).reset_connections();
    }
}

impl<T: HttpTransport + ?Sized> HttpTransport for Arc<T> {
    fn post_json(
        &self,
        request: HttpRequest<'_>,
    ) -> impl Future<Output = Result<Value, ClassifyError>> + Send {
        (**self).post_json(request)
    }

    fn reset_connections(&self) {
        (**self).reset_connections();
    }
}

/// The production transport, over `reqwest`.
#[derive(Debug, Default)]
pub struct ReqwestTransport {
    /// Behind a lock so [`reset_connections`](HttpTransport::reset_connections)
    /// can swap it for a fresh one. Every request clones the handle out (a
    /// `reqwest::Client` is an `Arc` inside, so the clone is cheap) and never
    /// holds the lock across an await.
    client: RwLock<reqwest::Client>,
}

impl ReqwestTransport {
    /// A transport with its own, empty connection pool.
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self) -> reqwest::Client {
        self.client
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl HttpTransport for ReqwestTransport {
    async fn post_json(&self, request: HttpRequest<'_>) -> Result<Value, ClassifyError> {
        let response = self
            .client()
            .post(request.url)
            .bearer_auth(request.api_key.expose())
            .timeout(request.timeout)
            .json(request.body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ClassifyError::Timeout(request.timeout.as_secs())
                } else {
                    ClassifyError::Transport(scrub_urls(&e.to_string()))
                }
            })?;
        // The status line has arrived, so the far side answered. Everything
        // below keeps that fact even when the body cannot be read: a 401 whose
        // body is cut off is still a rejected key (not an outage), and a 2xx
        // with an unreadable body still accepted it — which is all a probe asks.
        let status = response.status();
        let text = response.text().await;
        if !status.is_success() {
            return Err(ClassifyError::status(
                status.as_u16(),
                text.as_deref().unwrap_or_default(),
            ));
        }
        let text = text.map_err(|e| {
            ClassifyError::InvalidResponse(format!(
                "the response body could not be read: {}",
                scrub_urls(&e.to_string())
            ))
        })?;
        serde_json::from_str(&text).map_err(|e| ClassifyError::InvalidResponse(e.to_string()))
    }

    /// Replace the client, abandoning its connection pool (F-111).
    ///
    /// After the machine sleeps, the pool's keep-alive connections are
    /// half-open: the peer has long since dropped them, but nothing told this
    /// side. The pool's own idle timeout does not save us — it is measured on
    /// the monotonic clock, which did not advance while the machine was asleep
    /// — so the first request after waking is spent discovering the
    /// connection is dead (a fast reset if we are lucky, a full timeout if we
    /// are not). A new client starts empty and pays one TLS handshake instead.
    /// In-flight requests keep the handle they cloned and finish on it.
    fn reset_connections(&self) {
        *self
            .client
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = reqwest::Client::new();
    }
}

/// Strip credentials and query strings out of every URL in a message.
///
/// reqwest's error text carries the request URL, and the gateway URL is
/// operator-written: one configured as `https://user:pass@host/v1` or
/// `https://host/v1?key=…` would otherwise copy its secret into logs and
/// health files that no redacting layer sees. Scheme, host, port and path
/// survive, which is everything "which gateway" needs.
pub fn scrub_urls(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        // The URL runs to whitespace or a closing bracket.
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '}' | '>'))
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        let authority_end = url.find('/').unwrap_or(url.len());
        let url = match url[..authority_end].rfind('@') {
            Some(at) => &url[at + 1..],
            None => url,
        };
        let url = url.split(['?', '#']).next().unwrap_or(url);
        out.push_str(url);
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_credentials_and_query() {
        assert_eq!(
            scrub_urls(
                "error sending request for url (https://me:s3cret@gw.example/v1/chat/completions?key=abc#frag): dns error"
            ),
            "error sending request for url (https://gw.example/v1/chat/completions): dns error"
        );
        // No URL: untouched. A bare scheme: still fine.
        assert_eq!(scrub_urls("connection refused"), "connection refused");
        assert_eq!(scrub_urls("bad ://"), "bad ://");
    }

    #[test]
    fn the_api_key_never_debug_prints() {
        let key = ApiKey::new("sk-live-secret");
        assert!(!format!("{key:?}").contains("sk-live"));
        assert_eq!(key.expose(), "sk-live-secret");
    }

    /// Serve one canned HTTP response whose body is cut short: the headers
    /// promise 1000 bytes, 5 arrive, then the connection closes.
    fn truncated_server(status_line: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let response = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\nshort"
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
        });
        url
    }

    async fn post(url: &str) -> Result<Value, ClassifyError> {
        let key = ApiKey::default();
        let body = serde_json::json!({});
        ReqwestTransport::new()
            .post_json(HttpRequest {
                url,
                api_key: &key,
                body: &body,
                timeout: Duration::from_secs(5),
            })
            .await
    }

    /// The status line is the answer; a body that cannot be read must not
    /// turn a rejected key into an outage (`doctor --online`, F-111).
    #[tokio::test]
    async fn a_rejection_with_an_unreadable_body_is_still_an_auth_failure() {
        let err = post(&truncated_server("HTTP/1.1 401 Unauthorized"))
            .await
            .unwrap_err();
        assert!(err.is_auth_failure(), "{err:?}");
    }

    /// …and a 2xx with an unreadable body is an answer we cannot use, not a
    /// gateway that is down — which a probe reads as "the key was accepted".
    #[tokio::test]
    async fn a_success_with_an_unreadable_body_is_an_unusable_answer() {
        let err = post(&truncated_server("HTTP/1.1 200 OK"))
            .await
            .unwrap_err();
        assert!(matches!(err, ClassifyError::InvalidResponse(_)), "{err:?}");
        assert!(!err.is_unreachable(), "{err:?}");
    }

    /// The transport survives a reset mid-life: the next request simply uses
    /// the new client. Against a closed port, so no network is needed and the
    /// outcome is the same before and after.
    #[tokio::test]
    async fn reset_connections_leaves_the_transport_usable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://127.0.0.1:{}/v1",
            listener.local_addr().unwrap().port()
        );
        drop(listener);
        let transport = ReqwestTransport::new();
        let key = ApiKey::default();
        let body = serde_json::json!({});
        let request = HttpRequest {
            url: &url,
            api_key: &key,
            body: &body,
            timeout: Duration::from_secs(2),
        };

        let before = transport.post_json(request).await.unwrap_err();
        transport.reset_connections();
        let after = transport.post_json(request).await.unwrap_err();
        assert!(before.is_unreachable(), "{before}");
        assert!(after.is_unreachable(), "{after}");
    }
}
