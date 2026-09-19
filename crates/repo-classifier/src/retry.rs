//! [`RetryPolicy`]: exponential backoff over retryable failures (§5.3).

use std::future::Future;
use std::time::Duration;

use crate::ClassifyError;

/// The longest single wait, however many attempts have gone by.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How often, and how patiently, a retryable failure is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt (`0` = one attempt only).
    pub max_retries: u32,
    /// The first wait; it doubles each attempt, capped at 60s.
    pub backoff_base: Duration,
}

impl RetryPolicy {
    /// One attempt, no retries.
    pub const NONE: Self = Self {
        max_retries: 0,
        backoff_base: Duration::ZERO,
    };

    /// The Orchestrator's default: 3 retries from 500ms.
    pub const STANDARD: Self = Self {
        max_retries: 3,
        backoff_base: Duration::from_millis(500),
    };

    /// Run `attempt` until it succeeds, fails terminally, or the retries run
    /// out. Only [`ClassifyError::is_retryable`] failures are retried.
    pub async fn run<T, F, Fut>(&self, mut attempt: F) -> Result<T, ClassifyError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ClassifyError>>,
    {
        let mut retries = 0;
        loop {
            match attempt().await {
                Err(e) if e.is_retryable() && retries < self.max_retries => {
                    // Saturating pow/mul, then a ceiling: a large retry count
                    // must not overflow into a panic.
                    let delay = self
                        .backoff_base
                        .saturating_mul(2u32.saturating_pow(retries))
                        .min(MAX_BACKOFF);
                    tracing::warn!(attempt = retries, error = %e, "classifier call failed; retrying");
                    tokio::time::sleep(delay).await;
                    retries += 1;
                }
                outcome => return outcome,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn count_attempts(policy: RetryPolicy, error: ClassifyError) -> u32 {
        let calls = AtomicU32::new(0);
        let _ = policy
            .run(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                let error = error.clone();
                async move { Err::<(), _>(error) }
            })
            .await;
        calls.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn retryable_failures_are_retried_up_to_the_limit() {
        assert_eq!(
            count_attempts(RetryPolicy::STANDARD, ClassifyError::Timeout(1)).await,
            4
        );
        assert_eq!(
            count_attempts(RetryPolicy::NONE, ClassifyError::Timeout(1)).await,
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_failures_are_not_retried() {
        for terminal in [
            ClassifyError::status(401, ""),
            ClassifyError::status(400, ""),
            ClassifyError::InvalidResponse("x".into()),
        ] {
            assert_eq!(count_attempts(RetryPolicy::STANDARD, terminal).await, 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_after_a_failure_is_returned() {
        let calls = AtomicU32::new(0);
        let out = RetryPolicy::STANDARD
            .run(|| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        Err(ClassifyError::status(503, ""))
                    } else {
                        Ok(n)
                    }
                }
            })
            .await;
        assert_eq!(out.unwrap(), 1);
    }
}
