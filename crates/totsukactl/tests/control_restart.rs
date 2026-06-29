use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::control::{handle_reload, handle_restart};

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![],
        env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

#[tokio::test]
async fn handle_restart_increments_count_and_lands_ready() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let reg = Arc::new(Registry::new());
    reg.set_pid("orchestrator", Some(0x7fff_fffe), Some(chrono::Utc::now()))
        .await;
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let specs = vec![spec("orchestrator", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = RestartCfg {
        policy: RestartPolicy::OnDeadOnly,
        backoff_secs: vec![1],
        max_attempts: 3,
    };

    handle_restart(
        "orchestrator",
        reg.clone(),
        spawner,
        &specs,
        &paths,
        clock,
        &cfg,
        Duration::from_millis(10),
    )
    .await
    .unwrap();
    let e = reg.get("orchestrator").await.unwrap();
    assert_eq!(e.state, ChildState::Ready);
    assert_eq!(e.restart_count, 1);
    assert!(paths.child_pid("orchestrator").exists());
}

#[tokio::test]
async fn handle_reload_errors_when_pid_unknown() {
    let reg = Arc::new(Registry::new());
    let err = handle_reload("agent-adapter", reg).await.unwrap_err();
    assert!(matches!(err, totsukactl::error::TotsukactlError::Internal(_)));
}
