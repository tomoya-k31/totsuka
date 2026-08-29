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

/// The wait GitHub is asking for, or `None` when this response is not a
/// throttle.
///
/// GitHub returns **403 or 429** for both rate-limit kinds, so the status is
/// not enough — a bare 403 is an ordinary permission error and must never be
/// retried. The order below is GitHub's own documented decision tree:
/// `retry-after` first, then a spent budget via `x-ratelimit-remaining: 0` plus
/// `x-ratelimit-reset`, then a floor.
fn rate_limit_wait(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> Option<u64> {
    if status != reqwest::StatusCode::TOO_MANY_REQUESTS && status != reqwest::StatusCode::FORBIDDEN
    {
        return None;
    }
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
    // A 429 is a throttle whatever its headers say. A 403 without any of them
    // is a permission problem — retrying it would burn the budget and hide the
    // real cause.
    (status == reqwest::StatusCode::TOO_MANY_REQUESTS).then_some(FALLBACK_RETRY_AFTER_SECS)
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
                    GithubError::Timeout(self.timeout.as_secs())
                } else {
                    GithubError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GithubError::Unauthorized);
        }
        // Classify before `text()`, which consumes the response and with it the
        // headers the classification depends on.
        let throttled = rate_limit_wait(status, response.headers());
        let text = response
            .text()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
        if let Some(retry_after_secs) = throttled {
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
                    if slept + delay > self.retry_budget {
                        tracing::warn!(
                            error = %e, ?delay,
                            "retry delay would exceed the per-call retry budget; giving up"
                        );
                        return Err(e);
                    }
                    slept += delay;
                    tracing::warn!(attempt, error = %e, "github call failed; retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
