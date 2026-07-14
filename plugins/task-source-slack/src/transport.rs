//! Slack Web API transport: the seam between the plugin's logic and the
//! network.
//!
//! The server is generic over [`SlackTransport`] so `initialize`'s TokenGuard
//! (and the mention/reply flow) is exercised in tests against a
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
    /// with an optional JSON-object `body` of arguments, returning the parsed
    /// response JSON.
    ///
    /// `idempotent` guards automatic retries: a non-idempotent mutation
    /// (posting a message) passes `false` so a lost response never duplicates
    /// the write. Rate limiting (429) is retried either way — a throttled
    /// request was rejected, never applied.
    fn call(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, SlackError>> + Send;

    /// POST a JSON `body` to an absolute `url` outside the Web API base — the
    /// `response_url` rewrite channel for ephemeral messages. Unauthenticated
    /// (the URL itself is the capability) and never retried: the URL is valid
    /// for only 5 uses, so a lost response must not burn another one.
    fn post_url(
        &self,
        url: &str,
        body: Value,
    ) -> impl Future<Output = Result<(), SlackError>> + Send;
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

/// Flatten a JSON object of Web API arguments into form fields. Slack's read
/// methods (`conversations.replies`, `users.info`, …) do not accept a JSON
/// request body, but every method accepts `application/x-www-form-urlencoded`;
/// nested values (Block Kit `blocks`, …) are sent as their JSON text, which is
/// the encoding Slack documents for non-scalar form arguments.
fn form_fields(body: &Value) -> Result<Vec<(String, String)>, SlackError> {
    let Some(object) = body.as_object() else {
        return Err(SlackError::InvalidRequest(format!(
            "Web API arguments must be a JSON object, got: {body}"
        )));
    };
    let mut fields = Vec::with_capacity(object.len());
    for (key, value) in object {
        let text = match value {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            nested => nested.to_string(),
        };
        fields.push((key.clone(), text));
    }
    Ok(fields)
}

/// Capped exponential backoff delay for retry number `attempt` (0-based):
/// `base * 2^attempt`, never above `cap`. Shared by the HTTP retry loop and
/// the Socket Mode reconnect loop so the policy can only diverge on purpose.
pub(crate) fn capped_backoff(base: Duration, cap: Duration, attempt: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(attempt)).min(cap)
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
    retry_budget: Duration,
}

/// Ceiling for the *backoff-derived* delay (transient network / 5xx). A 429's
/// `Retry-After` is honored exactly instead — retrying earlier than asked is
/// guaranteed to be throttled again — and is bounded by the retry budget.
const MAX_BACKOFF_DELAY: Duration = Duration::from_secs(60);

/// Total sleep allowed across one call's retries. When honoring the next
/// delay (e.g. a long `Retry-After`) would exceed it, the call fails fast
/// with the real cause instead of appearing to hang — `initialize`'s
/// TokenGuard sits on this path, and the host must see an answer.
const DEFAULT_RETRY_BUDGET: Duration = Duration::from_secs(90);

/// The `Retry-After` to assume when the header is missing or unparseable
/// (e.g. the RFC 9110 HTTP-date form). Conservative on purpose: guessing low
/// would hammer an endpoint that just told us it is throttled.
const FALLBACK_RETRY_AFTER_SECS: u64 = 30;

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
            retry_budget: DEFAULT_RETRY_BUDGET,
        }
    }

    /// Override the retry timing (first backoff delay and total retry budget).
    /// Intended for tests, which should not pay real production-scale sleeps.
    pub fn with_retry_timing(mut self, backoff_base: Duration, retry_budget: Duration) -> Self {
        self.backoff_base = backoff_base;
        self.retry_budget = retry_budget;
        self
    }

    fn token(&self, kind: TokenKind) -> &str {
        match kind {
            TokenKind::App => &self.app_token,
            TokenKind::User => &self.user_token,
        }
    }

    /// Map a reqwest send failure to [`SlackError`].
    fn send_error(&self, e: reqwest::Error) -> SlackError {
        if e.is_timeout() {
            SlackError::Timeout(self.timeout.as_secs())
        } else {
            SlackError::Transport(e.to_string())
        }
    }

    /// Turn a non-2xx response into the matching [`SlackError`]: 429 becomes
    /// [`SlackError::RateLimited`] with its `Retry-After`, everything else
    /// [`SlackError::Http`] with a truncated body. `context` names the call
    /// for logs (Web API method, or the response_url).
    async fn status_error(context: &str, response: reqwest::Response) -> SlackError {
        let status = response.status();
        if status.as_u16() == 429 {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(FALLBACK_RETRY_AFTER_SECS);
            return SlackError::RateLimited {
                method: context.to_string(),
                retry_after_secs,
            };
        }
        let text = response.text().await.unwrap_or_default();
        SlackError::Http {
            status: status.as_u16(),
            body: text.chars().take(500).collect(),
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
        // (`auth.test`); arguments go as form fields (see [`form_fields`]).
        if let Some(body) = body {
            req = req.form(&form_fields(body)?);
        }

        let response = req.send().await.map_err(|e| self.send_error(e))?;
        if !response.status().is_success() {
            return Err(Self::status_error(method, response).await);
        }
        let text = response
            .text()
            .await
            .map_err(|e| SlackError::Transport(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| SlackError::InvalidResponse(e.to_string()))
    }

    /// The wait before retry number `attempt`: exactly what `Retry-After`
    /// asked for on a 429 (retrying earlier is guaranteed another 429; the
    /// retry budget bounds it), capped exponential backoff otherwise.
    fn retry_delay(&self, error: &SlackError, attempt: u32) -> Duration {
        if let SlackError::RateLimited {
            retry_after_secs, ..
        } = error
        {
            return Duration::from_secs(*retry_after_secs);
        }
        capped_backoff(self.backoff_base, MAX_BACKOFF_DELAY, attempt)
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
        let mut slept = Duration::ZERO;
        loop {
            match self.attempt(token, method, body.as_ref()).await {
                Ok(value) => return Ok(value),
                // Only replay when it is safe to: either the call is
                // idempotent, or the failure proves the request was rejected
                // (429) — a non-idempotent mutation whose response was lost
                // must surface the error, not re-run.
                Err(e)
                    if e.is_retryable()
                        && (idempotent || e.is_rejected())
                        && attempt < self.max_retries =>
                {
                    let delay = self.retry_delay(&e, attempt);
                    // Fail fast with the real cause once the budget is spent —
                    // sleeping minutes inside a call looks like a hang to the
                    // host (initialize's TokenGuard runs through here).
                    if slept + delay > self.retry_budget {
                        tracing::warn!(
                            method, error = %e, ?delay,
                            "retry delay would exceed the per-call retry budget; giving up"
                        );
                        return Err(e);
                    }
                    slept += delay;
                    tracing::warn!(attempt, method, error = %e, "slack call failed; retrying");
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn post_url(&self, url: &str, body: Value) -> Result<(), SlackError> {
        let response = self
            .client
            .post(url)
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| self.send_error(e))?;
        if !response.status().is_success() {
            // Label, not the URL itself: a response_url is a capability
            // secret and must never end up in error text or logs.
            return Err(Self::status_error("response_url", response).await);
        }
        Ok(())
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

    #[test]
    fn form_fields_flatten_scalars_and_json_encode_nested() {
        let fields = form_fields(&json!({
            "channel": "C1",
            "limit": 20,
            "unfurl_links": false,
            "skip_me": null,
            "blocks": [{ "type": "section" }],
        }))
        .unwrap();
        let get = |k: &str| {
            fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("channel"), Some("C1"));
        assert_eq!(get("limit"), Some("20"));
        assert_eq!(get("unfurl_links"), Some("false"));
        assert_eq!(get("blocks"), Some(r#"[{"type":"section"}]"#));
        assert_eq!(get("skip_me"), None);
    }

    #[test]
    fn form_fields_reject_non_object_arguments_as_a_request_bug() {
        let err = form_fields(&json!(["not", "an", "object"])).unwrap_err();
        assert!(matches!(err, SlackError::InvalidRequest(_)), "{err}");
        assert!(err.to_string().contains("plugin bug"), "{err}");
    }
}
