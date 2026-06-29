//! We don't actually fork in tests — we exercise the pre-fork pidfile guard.
use tempfile::TempDir;
use totsukactl::commands::up;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;
use totsukactl::pidfile;

const TOML: &str = include_str!("./fixtures/min_config.toml");

#[tokio::test]
async fn up_refuses_when_supervisor_already_running() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    // Write OUR own PID — process is alive so pidfile::check returns Alive.
    pidfile::write_pid(&paths.supervisor_pid(), std::process::id() as i32).unwrap();

    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let err = up::run(cfg, paths, false, false).await.unwrap_err();
    assert!(matches!(err, TotsukactlError::AlreadyRunning(_)));
}
