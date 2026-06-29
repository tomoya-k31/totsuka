use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;

#[tokio::test]
async fn up_then_ps_reflects_running() {
    let m = MockCompose::default();
    assert!(!m.ps_running("pgmq").await.unwrap());
    m.up_detached("pgmq", false).await.unwrap();
    assert!(m.ps_running("pgmq").await.unwrap());
    let calls = m.calls();
    assert!(calls.iter().any(|c| c == "up_detached:pgmq:false"));
}

#[tokio::test]
async fn inspect_image_returns_canned_value() {
    let m = MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1");
    assert_eq!(
        m.inspect_image("totsuka-pgmq").await.unwrap(),
        "ghcr.io/pgmq/pg18-pgmq:v1.11.1"
    );
}

#[tokio::test]
async fn docker_info_failure_surfaces() {
    let m = MockCompose::default();
    *m.fail_docker_info.lock().unwrap() = true;
    assert!(m.docker_info().await.is_err());
}
