//! The production [`DiscordTransport`]: reqwest, bot auth, and Discord's
//! rate-limit contract.

use std::time::Duration;

use serde_json::Value;

use crate::error::DiscordError;
use crate::transport::{
    DiscordTransport, HttpMethod, TransportSettings, capped_backoff, classify_status,
    retry_after_delay,
};

/// First backoff step for a retryable failure; doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(500);
/// Backoff ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Reqwest-backed transport.
pub struct ReqwestTransport {
    client: reqwest::Client,
    api_url: String,
    bot_token: String,
    max_retries: u32,
}

impl ReqwestTransport {
    /// Build from connection `settings`.
    pub fn new(settings: TransportSettings<'_>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_url: settings.api_url.trim_end_matches('/').to_string(),
            bot_token: settings.bot_token.to_string(),
            max_retries: settings.max_retries,
        }
    }
}

impl DiscordTransport for ReqwestTransport {
    async fn call(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> Result<Value, DiscordError> {
        let url = format!("{}{path}", self.api_url);
        let mut attempt: u32 = 0;
        loop {
            let request = match method {
                HttpMethod::Get => self.client.get(&url),
                HttpMethod::Post => self.client.post(&url),
            }
            // Discord's scheme word is `Bot`, not `Bearer`; a bearer header
            // is answered 401 with no hint that the word is the problem.
            .header("Authorization", format!("Bot {}", self.bot_token))
            .header(
                "User-Agent",
                "totsuka (https://github.com/tomoya-k31/totsuka, 0.1)",
            );
            let request = match &body {
                Some(json) => request.json(json),
                None => request,
            };

            let response = match request.send().await {
                Ok(response) => response,
                Err(e) => {
                    // A send that failed in flight may still have been
                    // delivered, so a non-idempotent call must not be
                    // replayed — a retried post posts twice.
                    if !idempotent || attempt >= self.max_retries {
                        return Err(DiscordError::Transport(e.to_string()));
                    }
                    tokio::time::sleep(capped_backoff(FIRST_BACKOFF, MAX_BACKOFF, attempt)).await;
                    attempt += 1;
                    continue;
                }
            };

            let status = response.status().as_u16();
            let parsed: Value = response.json().await.unwrap_or(Value::Null);

            // 429 is always safe to retry: the request was refused, not
            // performed, so even a message send can be sent again.
            if status == 429 {
                if attempt >= self.max_retries {
                    return Err(classify_status(path, status, &parsed));
                }
                let retry_after = parsed
                    .get("retry_after")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0);
                tracing::warn!(path, retry_after, "discord rate limited; backing off");
                tokio::time::sleep(retry_after_delay(retry_after)).await;
                attempt += 1;
                continue;
            }

            if (200..300).contains(&status) {
                return Ok(parsed);
            }

            let error = classify_status(path, status, &parsed);
            // 5xx is transient; 4xx is not, and retrying it only delays the
            // message that says what to fix.
            if status >= 500 && idempotent && attempt < self.max_retries {
                tokio::time::sleep(capped_backoff(FIRST_BACKOFF, MAX_BACKOFF, attempt)).await;
                attempt += 1;
                continue;
            }
            return Err(error);
        }
    }
}
