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
        return Err(SlackError::InvalidResponse(format!(
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

/// Longest single retry delay, whether from backoff or `Retry-After`.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

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
        // (`auth.test`); arguments go as form fields (see [`form_fields`]).
        if let Some(body) = body {
            req = req.form(&form_fields(body)?);
        }

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                SlackError::Timeout(self.timeout.as_secs())
            } else {
                SlackError::Transport(e.to_string())
            }
        })?;

        let status = response.status();
        if status.as_u16() == 429 {
            let retry_after_secs = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            return Err(SlackError::RateLimited {
                method: method.to_string(),
                retry_after_secs,
            });
        }
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

    /// The wait before retry number `attempt`: what `Retry-After` asked for on
    /// a 429, capped exponential backoff otherwise.
    fn retry_delay(&self, error: &SlackError, attempt: u32) -> Duration {
        if let SlackError::RateLimited {
            retry_after_secs, ..
        } = error
        {
            return Duration::from_secs(*retry_after_secs).min(MAX_RETRY_DELAY);
        }
        let factor = 2u32.saturating_pow(attempt);
        self.backoff_base
            .saturating_mul(factor)
            .min(MAX_RETRY_DELAY)
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
            .map_err(|e| {
                if e.is_timeout() {
                    SlackError::Timeout(self.timeout.as_secs())
                } else {
                    SlackError::Transport(e.to_string())
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(SlackError::Http {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
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
    fn form_fields_reject_non_object_arguments() {
        assert!(form_fields(&json!(["not", "an", "object"])).is_err());
    }
}
