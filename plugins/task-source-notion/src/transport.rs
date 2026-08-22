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
            return Err(NotionError::Http {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
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
