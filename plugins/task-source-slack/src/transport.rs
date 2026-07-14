//! Slack Web API transport: the seam between the plugin's logic and the
//! network.
//!
//! The server is generic over [`SlackTransport`] so `initialize`'s TokenGuard
//! (and, later, the mention/reply flow) is exercised in tests against a
//! recorded-response fake, while production uses [`ReqwestTransport`] (bearer
//! auth per token kind and capped exponential backoff on retryable failures).

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::SlackError;

/// Which of the two configured tokens authenticates a Web API call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// The App-Level Token (`xapp-`) — Socket Mode connection management.
    App,
    /// The user token (`xoxp-`) — everything posted as the operator.
    User,
}

/// Connection settings for building a transport. Grouped so factories and
/// constructors take one argument rather than a long positional list.
#[derive(Debug, Clone, Copy)]
pub struct TransportSettings<'a> {
    /// Web API base URL (no trailing slash), e.g. `https://slack.com/api`.
    pub api_url: &'a str,
    /// App-Level Token (`xapp-`).
    pub app_token: &'a str,
    /// User token (`xoxp-`).
    pub user_token: &'a str,
    /// Max retry attempts for retryable failures.
    pub max_retries: u32,
}

/// Sends a Slack Web API request and returns the parsed JSON body.
///
/// The transport handles HTTP-level concerns only (status, retries, JSON
/// parsing); Slack's application-level `ok`/`error` envelope is the caller's
/// to interpret (see [`expect_ok`]).
pub trait SlackTransport: Send + Sync {
    /// Perform Web API `method` (e.g. `auth.test`) authenticated by `token`,
    /// with an optional JSON `body`, returning the parsed response JSON.
    ///
    /// `idempotent` guards automatic retries: a non-idempotent mutation
    /// (posting a message) passes `false` so a lost response never duplicates
    /// the write.
    fn call(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, SlackError>> + Send;
}

/// Unwrap Slack's `{"ok": bool, ...}` envelope: the response itself on
/// success, [`SlackError::Api`] carrying Slack's error code otherwise.
pub fn expect_ok(method: &str, response: Value) -> Result<Value, SlackError> {
    match response.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(response),
        Some(false) => Err(SlackError::Api {
            method: method.to_string(),
            error: response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
                .to_string(),
        }),
        None => Err(SlackError::InvalidResponse(format!(
            "`{method}` response has no boolean `ok` field"
        ))),
    }
}

/// The production transport: reqwest against Slack's Web API.
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
    app_token: String,
    user_token: String,
    timeout: Duration,
    max_retries: u32,
    backoff_base: Duration,
}

impl ReqwestTransport {
    /// A transport from connection `settings`.
    pub fn new(settings: TransportSettings<'_>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: settings.api_url.trim_end_matches('/').to_string(),
            app_token: settings.app_token.to_string(),
            user_token: settings.user_token.to_string(),
            timeout: Duration::from_secs(30),
            max_retries: settings.max_retries,
            backoff_base: Duration::from_millis(500),
        }
    }

    fn token(&self, kind: TokenKind) -> &str {
        match kind {
            TokenKind::App => &self.app_token,
            TokenKind::User => &self.user_token,
        }
    }

    /// One HTTP attempt, mapping transport/status failures to [`SlackError`].
    async fn attempt(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<&Value>,
    ) -> Result<Value, SlackError> {
        let url = format!("{}/{}", self.base_url, method);
        let mut req = self
            .client
            .post(&url)
            .bearer_auth(self.token(token))
            .timeout(self.timeout);
        // Slack's Web API accepts an empty POST for no-argument methods
        // (`auth.test`); a JSON body is sent only when there are arguments.
        if let Some(body) = body {
            req = req.json(body);
        }

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                SlackError::Timeout(self.timeout.as_secs())
            } else {
                SlackError::Transport(e.to_string())
            }
        })?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| SlackError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(SlackError::Http {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        serde_json::from_str(&text).map_err(|e| SlackError::InvalidResponse(e.to_string()))
    }
}

impl SlackTransport for ReqwestTransport {
    async fn call(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> Result<Value, SlackError> {
        let mut attempt = 0;
        loop {
            match self.attempt(token, method, body.as_ref()).await {
                Ok(value) => return Ok(value),
                // Only replay when it is safe to: a non-idempotent mutation
                // whose response was lost must surface the error, not re-run.
                Err(e) if idempotent && e.is_retryable() && attempt < self.max_retries => {
                    let factor = 2u32.saturating_pow(attempt);
                    let delay = self
                        .backoff_base
                        .saturating_mul(factor)
                        .min(Duration::from_secs(60));
                    tracing::warn!(attempt, method, error = %e, "slack call failed; retrying");
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
    use serde_json::json;

    #[test]
    fn expect_ok_passes_success_through() {
        let resp = expect_ok("auth.test", json!({ "ok": true, "user_id": "U1" })).unwrap();
        assert_eq!(resp["user_id"], "U1");
    }

    #[test]
    fn expect_ok_maps_error_code() {
        let err = expect_ok(
            "auth.test",
            json!({ "ok": false, "error": "missing_scope" }),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SlackError::Api { ref method, ref error }
                if method == "auth.test" && error == "missing_scope"
        ));
    }

    #[test]
    fn expect_ok_rejects_envelope_without_ok() {
        let err = expect_ok("auth.test", json!({ "user_id": "U1" })).unwrap_err();
        assert!(matches!(err, SlackError::InvalidResponse(_)));
    }
}
