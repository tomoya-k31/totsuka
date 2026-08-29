//! GraphQL transport: the seam between the plugin's logic and the network.
//!
//! [`GithubClient`](crate::client::GithubClient) is generic over this trait so
//! its fetch/update/publish/validate logic is exercised in tests against a
//! recorded-response fake, while production uses [`ReqwestTransport`].

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::GithubError;

/// Sends a GraphQL request body and returns the parsed JSON `data` envelope.
pub trait GithubTransport: Send + Sync {
    /// POST `body` (`{ "query": ..., "variables": ... }`) to the GraphQL API and
    /// return the full response JSON (including any top-level `errors`).
    ///
    /// `idempotent` guards automatic retries: a timed-out/5xx request is only
    /// re-sent when replaying it is safe. **A throttle is outside that guard**
    /// — the request provably never ran, so it is replayed regardless of the
    /// flag. **Every current caller passes `true`**
    /// — setting a project field to a value gives the same result however many
    /// times it lands, and the one call that could duplicate (`addComment`)
    /// went with `result/publish` (#398). The flag stays because a future
    /// create-shaped mutation would need it, and finding that out from a
    /// duplicated side effect is expensive.
    fn post_graphql(
        &self,
        body: Value,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, GithubError>> + Send;
}

/// Ceiling for the *backoff-derived* delay (transient network / 5xx). A
/// throttle's `retry-after` is honoured exactly instead — retrying earlier than
/// GitHub asked is guaranteed to be throttled again, and it penalises clients
/// that do — bounded only by the retry budget.
const MAX_BACKOFF_DELAY: Duration = Duration::from_secs(60);

/// Total sleep allowed across one call's retries. When honouring the next delay
/// would exceed it, the call fails fast with the real cause instead of
/// appearing to hang: `poll_loop` drives this path, and a tick that sleeps for
/// minutes is indistinguishable from a wedged plugin.
const DEFAULT_RETRY_BUDGET: Duration = Duration::from_secs(90);

/// The wait to assume for a throttle that carries no usable header. GitHub's
/// own guidance is "wait for at least one minute before retrying"; guessing
/// lower would hammer an endpoint that just said it is throttled.
const FALLBACK_RETRY_AFTER_SECS: u64 = 60;

/// The production transport: reqwest against GitHub's GraphQL API with bearer
/// auth, a required User-Agent, timeouts, `retry-after`-aware throttling, and
/// capped exponential backoff (§5.3).
pub struct ReqwestTransport {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    timeout: Duration,
    max_retries: u32,
    backoff_base: Duration,
    retry_budget: Duration,
}

/// Whether this response body says we were rate limited.
///
/// **This, not the status code, is the discriminator.** On the GraphQL API a
/// primary rate limit comes back as **HTTP 200** with an error message, and a
/// secondary one as **200 or 403** — so a status-only test misses the common
/// case entirely, and cannot tell a throttled 403 from a permission error.
fn body_says_rate_limited(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let mentions = |s: &str| s.to_ascii_lowercase().contains("rate limit");
    // GraphQL: `errors[].type == "RATE_LIMITED"`, or a message naming it —
    // that is how the secondary limit announces itself.
    if let Some(errors) = value["errors"].as_array()
        && errors.iter().any(|e| {
            e["type"].as_str() == Some("RATE_LIMITED")
                || e["message"].as_str().is_some_and(mentions)
        })
    {
        return true;
    }
    // The REST-shaped error body a 403 can carry.
    value["message"].as_str().is_some_and(mentions)
}

/// The wait the headers ask for, if they say anything usable.
///
/// GitHub's documented order: `retry-after` first, then a spent budget via
/// `x-ratelimit-remaining: 0` plus the epoch-seconds `x-ratelimit-reset`.
fn wait_from_headers(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    if let Some(secs) = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(secs);
    }
    // `x-ratelimit-reset` is UTC epoch seconds, so it becomes a wait only
    // relative to now — and a reset already in the past means "no wait".
    if header("x-ratelimit-remaining").is_some_and(|v| v.trim() == "0")
        && let Some(reset) = header("x-ratelimit-reset").and_then(|v| v.trim().parse::<u64>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return Some(reset.saturating_sub(now));
    }
    None
}

/// The wait GitHub is asking for, or `None` when this response is not a
/// throttle.
///
/// A 429 is always one. Anything else — **including a 200** — is one only when
/// the body says so, which is what keeps an ordinary 403 (insufficient scopes)
/// and an ordinary GraphQL error out of the retry path.
///
/// The result is floored at one second: `retry-after: 0`, a reset instant
/// already past, or a skewed clock would otherwise produce a zero delay and
/// turn the budget into `max_retries` back-to-back requests — the exact
/// hammering the fallback exists to prevent.
fn rate_limit_wait(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> Option<u64> {
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS && !body_says_rate_limited(body) {
        return None;
    }
    Some(
        wait_from_headers(headers)
            .unwrap_or(FALLBACK_RETRY_AFTER_SECS)
            .max(1),
    )
}

impl ReqwestTransport {
    /// A transport for `endpoint`, authenticating with `token`.
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>, max_retries: u32) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
            token: token.into(),
            timeout: Duration::from_secs(30),
            max_retries,
            backoff_base: Duration::from_millis(500),
            retry_budget: DEFAULT_RETRY_BUDGET,
        }
    }

    /// Override the retry timing (first backoff delay and total retry budget).
    /// Intended for tests, which should not pay production-scale sleeps.
    pub fn with_retry_timing(mut self, backoff_base: Duration, retry_budget: Duration) -> Self {
        self.backoff_base = backoff_base;
        self.retry_budget = retry_budget;
        self
    }

    /// Override the request timeout. Intended for tests: the production value
    /// is 30s, and no test can afford to wait that out — which is why the
    /// timeout → `Timeout` mapping went unpinned until this existed.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The configured timeout in whole seconds, **rounded up**.
    ///
    /// `Duration::as_secs` truncates, so anything under a second would be
    /// reported as `timed out after 0s` — a message that reads like a bug in
    /// the reporting rather than a timeout. Production is 30s and never sees
    /// this; [`with_timeout`](Self::with_timeout) is what makes sub-second
    /// values reachable, so the rounding belongs next to it.
    fn timeout_secs(&self) -> u64 {
        self.timeout.as_millis().div_ceil(1000) as u64
    }

    /// The wait before retry number `attempt`: exactly what GitHub asked for on
    /// a throttle, capped exponential backoff otherwise.
    fn retry_delay(&self, error: &GithubError, attempt: u32) -> Duration {
        if let GithubError::RateLimited { retry_after_secs } = error {
            return Duration::from_secs(*retry_after_secs);
        }
        self.backoff_base
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(MAX_BACKOFF_DELAY)
    }

    /// One HTTP attempt, mapping transport/status failures to [`GithubError`].
    async fn attempt(&self, body: &Value) -> Result<Value, GithubError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            // GitHub rejects GraphQL requests without a User-Agent.
            .header(reqwest::header::USER_AGENT, "totsuka-task-source-github")
            .timeout(self.timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    GithubError::Timeout(self.timeout_secs())
                } else {
                    GithubError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GithubError::Unauthorized);
        }
        // The headers must be taken before `text()`, which consumes the
        // response; the body is the other half of the classification.
        let headers = response.headers().clone();
        let text = response
            .text()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        if let Some(retry_after_secs) = rate_limit_wait(status, &headers, &text) {
            return Err(GithubError::RateLimited { retry_after_secs });
        }
        if !status.is_success() {
            return Err(GithubError::Http {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        serde_json::from_str(&text).map_err(|e| GithubError::InvalidResponse(e.to_string()))
    }
}

impl GithubTransport for ReqwestTransport {
    async fn post_graphql(&self, body: Value, idempotent: bool) -> Result<Value, GithubError> {
        let mut attempt = 0;
        let mut slept = Duration::ZERO;
        loop {
            match self.attempt(&body).await {
                Ok(value) => return Ok(value),
                // Only replay when it is safe to: either the call is
                // idempotent, or the failure proves the request was rejected
                // (a throttle never ran it) — a non-idempotent mutation whose
                // response was merely lost must surface the error, not re-run.
                Err(e)
                    if e.is_retryable()
                        && (idempotent || e.is_rejected())
                        && attempt < self.max_retries =>
                {
                    let delay = self.retry_delay(&e, attempt);
                    // Fail fast with the real cause once the budget is spent:
                    // sleeping for minutes inside one `poll_loop` tick looks
                    // like a wedged plugin from the outside.
                    if slept.saturating_add(delay) > self.retry_budget {
                        tracing::warn!(
                            error = %e, ?delay,
                            "retry delay would exceed the per-call retry budget; giving up"
                        );
                        return Err(e);
                    }
                    slept = slept.saturating_add(delay);
                    tracing::warn!(attempt, error = %e, "github call failed; retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
