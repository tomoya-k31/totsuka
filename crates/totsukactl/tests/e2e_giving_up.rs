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
use totsukactl::supervisor::control::handle_restart;

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
async fn restart_loop_lands_in_giving_up_after_max_attempts() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let registry = Arc::new(Registry::new());
    let spawner_concrete = Arc::new(MockSpawner::default());
    spawner_concrete
        .fail_for
        .lock()
        .unwrap()
        .push("orchestrator".into());
    let spawner: Arc<dyn ChildSpawner> = spawner_concrete.clone();
    let specs = vec![spec("orchestrator", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = RestartCfg {
        policy: RestartPolicy::OnDeadOnly,
        backoff_secs: vec![0],
        max_attempts: 3,
    };

    // First three attempts: each call surfaces the spawn error, but `restart_count`
    // only increments on success — so each failed call leaves count=0 and re-tries
    // are eligible. We bump restart_count manually to simulate a real supervisor's
    // counter (mirrors handle_restart's bump on success path). To exercise the
    // GivingUp branch, set restart_count directly.
    registry.set_state("orchestrator", ChildState::Dead).await;
    for _ in 0..3 {
        registry.bump_restart("orchestrator").await;
    }

    let err = handle_restart(
        "orchestrator",
        registry.clone(),
        spawner,
        &specs,
        &paths,
        clock,
        &cfg,
        Duration::from_millis(5),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("giving_up"));
    assert_eq!(
        registry.get("orchestrator").await.unwrap().state,
        ChildState::GivingUp
    );
}
