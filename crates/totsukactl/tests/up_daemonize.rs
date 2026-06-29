//! We don't actually fork in tests — we exercise the pre-fork pidfile guard.
use std::sync::Mutex;
use tempfile::TempDir;
use totsukactl::commands::up;
use totsukactl::error::TotsukactlError;
use totsukactl::paths::Paths;
use totsukactl::pidfile;

// XDG_CONFIG_HOME / HOME are process-global; serialize tests that touch them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn bootstrap_returns_error_when_compose_unavailable() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", tmp.path());
    std::env::set_var("HOME", tmp.path());
    let paths = Paths {
        state_dir: tmp.path().join("state"),
        data_dir: tmp.path().join("data"),
        log_dir: tmp.path().join("state/logs"),
        pid_dir: tmp.path().join("state/pids"),
        sock_dir: tmp.path().join("state/sock"),
    };
    paths.ensure().unwrap();
    let cfg = totsuka_config::Config::from_toml_str(TOML).unwrap();
    let err = up::run(cfg, paths, false, true).await.unwrap_err();
    let s = format!("{err:?}");
    assert!(
        matches!(
            err,
            TotsukactlError::Compose(_)
                | TotsukactlError::Migrate(_)
                | TotsukactlError::Probe(_)
                | TotsukactlError::Config(_)
        ),
        "expected init-path error, got {s}"
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("HOME");
}
