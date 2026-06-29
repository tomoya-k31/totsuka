use std::sync::Arc;
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::probe::{ensure_image_match, pgmq_compatible};

#[test]
fn pgmq_compatible_accepts_patch_drift() {
    assert!(pgmq_compatible("1.11.1", "1.11.1"));
    assert!(pgmq_compatible("1.11.2", "1.11.1"));
    assert!(pgmq_compatible("1.11.0", "1.11.1"));
}

#[test]
fn pgmq_compatible_rejects_minor_drift() {
    assert!(!pgmq_compatible("1.10.1", "1.11.1"));
    assert!(!pgmq_compatible("2.0.0", "1.11.1"));
    assert!(!pgmq_compatible("garbage", "1.11.1"));
}

#[tokio::test]
async fn image_match_ok_passes() {
    let m: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1"));
    ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", false)
        .await
        .unwrap();
}

#[tokio::test]
async fn image_mismatch_without_recreate_errors() {
    let m: Arc<dyn ComposeExec> = Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.10.0"));
    let err = ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("image mismatch"));
}

#[tokio::test]
async fn image_mismatch_with_recreate_calls_up() {
    let inner: Arc<MockCompose> =
        Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.10.0"));
    let m: Arc<dyn ComposeExec> = inner.clone();
    ensure_image_match(m.as_ref(), "totsuka-pgmq", "ghcr.io/pgmq/pg18-pgmq:v1.11.1", true)
        .await
        .unwrap();
    assert!(inner.calls().iter().any(|c| c == "up_detached:pgmq:true"));
}
