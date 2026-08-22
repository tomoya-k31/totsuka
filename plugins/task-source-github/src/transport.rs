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
    /// re-sent when replaying it is safe. **Every current caller passes `true`**
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

/// The production transport: reqwest against GitHub's GraphQL API with bearer
/// auth, a required User-Agent, timeouts, and capped exponential backoff (§5.3).
pub struct ReqwestTransport {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    timeout: Duration,
    max_retries: u32,
    backoff_base: Duration,
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
        }
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
        let text = response
            .text()
            .await
            .map_err(|e| GithubError::Transport(e.to_string()))?;
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
        loop {
            match self.attempt(&body).await {
                Ok(value) => return Ok(value),
                // Only replay when it is safe to: a non-idempotent mutation
                // whose response was lost must surface the error, not re-run.
                Err(e) if idempotent && e.is_retryable() && attempt < self.max_retries => {
                    let factor = 2u32.saturating_pow(attempt);
                    let delay = self
                        .backoff_base
                        .saturating_mul(factor)
                        .min(Duration::from_secs(60));
                    tracing::warn!(attempt, error = %e, "github call failed; retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
