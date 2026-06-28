use crate::error::QaError;
use std::future::Future;
use std::time::Duration;

pub async fn with_classify_retry<F, Fut, T>(max_attempts: u32, mut op: F) -> Result<T, QaError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, QaError>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < max_attempts && is_retryable(&e) => {
                let backoff = backoff_secs(attempt);
                tracing::warn!(error=%e, attempt, "classifier retrying in {backoff}s");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_retryable(e: &QaError) -> bool {
    matches!(e, QaError::Http(_))
        || matches!(e, QaError::Classifier(s) if s.contains("429") || s.contains("5") )
}

fn backoff_secs(attempt: u32) -> u64 {
    let s: u64 = 4u64.saturating_pow(attempt.saturating_sub(1));
    s.min(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_after_one_retryable_error() {
        let calls = AtomicU32::new(0);
        let r: u32 = with_classify_retry(3, || async {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 { Err(QaError::Classifier("500 internal".into())) } else { Ok(42) }
        }).await.unwrap();
        assert_eq!(r, 42);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let r: Result<u32, _> = with_classify_retry(2, || async {
            Err::<u32, _>(QaError::Classifier("500 internal".into()))
        }).await;
        assert!(matches!(r, Err(QaError::Classifier(_))));
    }
}
