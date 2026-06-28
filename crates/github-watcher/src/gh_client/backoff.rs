//! Centralised retry / rate-limit logic. Wrap any GhClient call that talks to
//! GitHub through `with_retry` so individual call sites stay clean.

use crate::error::WatcherError;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::{header, StatusCode};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

pub fn classify_http(
    status: StatusCode,
    headers: &header::HeaderMap,
    now: DateTime<Utc>,
) -> Option<WatcherError> {
    if status.is_success() {
        return None;
    }
    if status == StatusCode::FORBIDDEN
        && headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            == Some("0")
    {
        let reset_at = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
            .unwrap_or(now + chrono::Duration::seconds(30));
        return Some(WatcherError::RateLimited { reset_at });
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let secs = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(5);
        return Some(WatcherError::RateLimited {
            reset_at: now + chrono::Duration::seconds(secs),
        });
    }
    if status.is_server_error() {
        return Some(WatcherError::Internal(format!("REST {status}")));
    }
    Some(WatcherError::Internal(format!("REST {status}")))
}

pub async fn with_retry<T, F, Fut>(
    clock: Arc<dyn Clock>,
    max_attempts: u32,
    mut op: F,
) -> Result<T, WatcherError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, WatcherError>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(WatcherError::RateLimited { reset_at }) => {
                let now = clock.now();
                let wait = (reset_at - now).num_seconds().clamp(0, 30) as u64;
                tracing::warn!(reset_at=%reset_at, "rate-limited; sleeping {wait}s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                if attempt >= max_attempts {
                    return Err(WatcherError::RateLimited { reset_at });
                }
            }
            Err(e) if attempt < max_attempts && is_retryable(&e) => {
                let backoff = backoff_secs(attempt);
                tracing::warn!(error=%e, "retrying in {backoff}s (attempt {attempt})");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_retryable(e: &WatcherError) -> bool {
    matches!(e, WatcherError::Http(_))
        || matches!(e, WatcherError::Internal(s) if s.starts_with("REST 5"))
}

fn backoff_secs(attempt: u32) -> u64 {
    // 1, 4, 16, cap 30
    let s = 4u64.saturating_pow(attempt.saturating_sub(1));
    s.min(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use reqwest::header::HeaderMap;
    use totsuka_core::SystemClock;

    #[test]
    fn classify_rate_limited_403() {
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        h.insert("x-ratelimit-reset", "1762000000".parse().unwrap());
        let now = Utc.with_ymd_and_hms(2026, 6, 28, 0, 0, 0).unwrap();
        let e = classify_http(StatusCode::FORBIDDEN, &h, now).unwrap();
        assert!(matches!(e, WatcherError::RateLimited { .. }));
    }

    #[test]
    fn classify_5xx_internal() {
        let h = HeaderMap::new();
        let now = Utc.with_ymd_and_hms(2026, 6, 28, 0, 0, 0).unwrap();
        let e = classify_http(StatusCode::INTERNAL_SERVER_ERROR, &h, now).unwrap();
        assert!(matches!(e, WatcherError::Internal(_)));
    }

    #[tokio::test]
    async fn with_retry_succeeds_after_one_500() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let r = with_retry(Arc::new(SystemClock), 3, || async {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Err(WatcherError::Internal("REST 503".into()))
            } else {
                Ok(42u32)
            }
        })
        .await
        .unwrap();
        assert_eq!(r, 42);
    }
}
