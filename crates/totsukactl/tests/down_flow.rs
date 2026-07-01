use tempfile::TempDir;
use totsukactl::commands::down;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;

#[tokio::test]
async fn down_returns_not_running_without_pidfile() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let err = down::run(&paths, false, false, std::time::Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(matches!(err, TotsukactlError::NotRunning));
}
