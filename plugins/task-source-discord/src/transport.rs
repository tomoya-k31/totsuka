//! REST transport: the seam between the plugin's logic and the network, so
//! [`DiscordApi`](crate::discord_api::DiscordApi) is exercised in tests
//! against recorded responses with no network involved.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::error::{DiscordError, auth_failure};

/// HTTP method for a Discord REST call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET` — read a channel, its messages, the current user.
    Get,
    /// `POST` — send a message, start a thread.
    Post,
}

/// Connection settings for building a transport.
#[derive(Debug, Clone, Copy)]
pub struct TransportSettings<'a> {
    /// REST base URL with no trailing slash, e.g. `https://discord.com/api/v10`.
    pub api_url: &'a str,
    /// The bot token, without the `Bot ` prefix (the transport adds it).
    pub bot_token: &'a str,
    /// Max retry attempts for retryable failures.
    pub max_retries: u32,
}

/// Sends a Discord REST request and returns the parsed JSON body.
pub trait DiscordTransport: Send + Sync {
    /// Call `path` (relative to the API base, leading slash included).
    ///
    /// `idempotent` decides whether a failed attempt may be replayed: a
    /// `GET` may, a message send may not — a retried send that actually
    /// succeeded the first time posts twice.
    fn call(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, DiscordError>> + Send;
}

/// How long to wait after a 429, given Discord's `retry_after` (seconds).
///
/// Discord answers a rate limit with the exact wait it wants, so this honours
/// it rather than guessing — but caps it, because a `retry_after` long enough
/// to matter means backing off entirely is the right move and the caller's
/// attempt budget should end rather than sleep through it.
pub fn retry_after_delay(retry_after_secs: f64) -> Duration {
    const CAP: Duration = Duration::from_secs(30);
    if !retry_after_secs.is_finite() || retry_after_secs <= 0.0 {
        return Duration::from_millis(500);
    }
    Duration::from_secs_f64(retry_after_secs).min(CAP)
}

/// Capped exponential backoff for attempt `n` (0-based).
pub fn capped_backoff(base: Duration, max: Duration, n: u32) -> Duration {
    let factor = 1u64 << n.min(16);
    base.saturating_mul(factor as u32).min(max)
}

/// Turn a non-success HTTP status into the right error, with guidance for the
/// credential-class ones.
pub fn classify_status(path: &str, status: u16, body: &Value) -> DiscordError {
    if matches!(status, 401 | 403) {
        return auth_failure(status);
    }
    DiscordError::Api {
        method: path.to_string(),
        status,
        detail: body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retry_after_is_honoured_but_capped() {
        assert_eq!(retry_after_delay(1.5), Duration::from_secs_f64(1.5));
        assert_eq!(retry_after_delay(600.0), Duration::from_secs(30));
        // Nonsense values fall back rather than sleeping forever or not at all.
        assert_eq!(retry_after_delay(0.0), Duration::from_millis(500));
        assert_eq!(retry_after_delay(-1.0), Duration::from_millis(500));
        assert_eq!(retry_after_delay(f64::NAN), Duration::from_millis(500));
    }

    #[test]
    fn backoff_doubles_and_saturates() {
        let base = Duration::from_secs(1);
        let max = Duration::from_secs(30);
        assert_eq!(capped_backoff(base, max, 0), Duration::from_secs(1));
        assert_eq!(capped_backoff(base, max, 2), Duration::from_secs(4));
        assert_eq!(capped_backoff(base, max, 20), max, "no overflow, no wrap");
    }

    #[test]
    fn unauthorized_and_forbidden_carry_guidance_others_carry_the_detail() {
        assert!(classify_status("/users/@me", 401, &json!({})).is_credential());
        assert!(classify_status("/channels/1", 403, &json!({})).is_credential());

        let server = classify_status("/channels/1", 500, &json!({ "message": "boom" }));
        assert!(!server.is_credential());
        assert!(server.to_string().contains("boom"), "{server}");
    }
}
