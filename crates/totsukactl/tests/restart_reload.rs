use tempfile::TempDir;
use totsukactl::commands::{reload, restart};
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

fn paths(tmp: &TempDir) -> Paths {
    let p = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    p.ensure().unwrap();
    p
}

#[tokio::test]
async fn restart_pgmq_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "pgmq").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Config(_)));
}

#[tokio::test]
async fn restart_unknown_bin_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "nope").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::UnknownChild(_)));
}

#[tokio::test]
async fn reload_non_adapter_rejected() {
    let tmp = TempDir::new().unwrap();
    let err = reload::run(&paths(&tmp), "orchestrator").await.unwrap_err();
    assert!(matches!(err, TotsukactlError::Config(_)));
}

#[tokio::test]
async fn restart_adapter_without_supervisor_returns_not_running() {
    let tmp = TempDir::new().unwrap();
    let err = restart::run(&paths(&tmp), "agent-adapter")
        .await
        .unwrap_err();
    assert!(matches!(err, TotsukactlError::NotRunning));
}

#[tokio::test]
async fn reload_adapter_without_supervisor_returns_not_running() {
    let tmp = TempDir::new().unwrap();
    let err = reload::run(&paths(&tmp), "agent-adapter")
        .await
        .unwrap_err();
    assert!(matches!(err, TotsukactlError::NotRunning));
}
