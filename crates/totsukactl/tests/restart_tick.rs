use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::restart_tick::run_restart_tick;

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![],
        env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

fn make_paths(tmp: &TempDir) -> Paths {
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    paths
}

#[tokio::test]
async fn dead_process_gets_respawned_after_tick() {
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(&tmp);

    let registry = Arc::new(Registry::new());
    registry.set_state("agent-adapter", ChildState::Dead).await;

    let mock = Arc::new(MockSpawner::default());
    let spawner: Arc<dyn ChildSpawner> = mock.clone();
    let specs = vec![spec("agent-adapter", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let restart_cfg = RestartCfg {
        policy: RestartPolicy::OnDeadOnly,
        backoff_secs: vec![0],
        max_attempts: 5,
    };
    let shutdown = CancellationToken::new();

    let interval = Duration::from_millis(20);
    let handle = tokio::spawn(run_restart_tick(
        interval,
        registry.clone(),
        spawner,
        specs,
        paths,
        clock,
        restart_cfg,
        Duration::from_millis(1),
        shutdown.clone(),
    ));

    // Give it enough time for at least one tick.
    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.cancel();
    let _ = handle.await;

    let spawned = mock.spawned.lock().unwrap();
    assert!(
        spawned.contains(&"agent-adapter".to_string()),
        "expected agent-adapter to be respawned, got {:?}",
        *spawned
    );
}

#[tokio::test]
async fn pgmq_is_never_auto_restarted() {
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(&tmp);

    let registry = Arc::new(Registry::new());
    registry.set_state("pgmq", ChildState::Dead).await;

    let mock = Arc::new(MockSpawner::default());
    let spawner: Arc<dyn ChildSpawner> = mock.clone();
    // Even with a spec present, pgmq must be skipped.
    let specs = vec![spec("pgmq", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let restart_cfg = RestartCfg {
        policy: RestartPolicy::OnDeadOnly,
        backoff_secs: vec![0],
        max_attempts: 5,
    };
    let shutdown = CancellationToken::new();

    let interval = Duration::from_millis(20);
    let handle = tokio::spawn(run_restart_tick(
        interval,
        registry.clone(),
        spawner,
        specs,
        paths,
        clock,
        restart_cfg,
        Duration::from_millis(1),
        shutdown.clone(),
    ));

    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.cancel();
    let _ = handle.await;

    let spawned = mock.spawned.lock().unwrap();
    assert!(
        !spawned.contains(&"pgmq".to_string()),
        "pgmq must never be auto-restarted, got {:?}",
        *spawned
    );
}

#[tokio::test]
async fn stopped_process_is_not_restarted_by_default() {
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(&tmp);

    let registry = Arc::new(Registry::new());
    // Stopped state: OnDeadOnly policy should not restart it.
    registry
        .set_state("orchestrator", ChildState::Stopped)
        .await;

    let mock = Arc::new(MockSpawner::default());
    let spawner: Arc<dyn ChildSpawner> = mock.clone();
    let specs = vec![spec("orchestrator", &tmp)];
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let restart_cfg = RestartCfg {
        policy: RestartPolicy::OnDeadOnly,
        backoff_secs: vec![0],
        max_attempts: 5,
    };
    let shutdown = CancellationToken::new();

    let handle = tokio::spawn(run_restart_tick(
        Duration::from_millis(20),
        registry.clone(),
        spawner,
        specs,
        paths,
        clock,
        restart_cfg,
        Duration::from_millis(1),
        shutdown.clone(),
    ));

    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.cancel();
    let _ = handle.await;

    let spawned = mock.spawned.lock().unwrap();
    assert!(
        spawned.is_empty(),
        "stopped process should not be auto-restarted, got {:?}",
        *spawned
    );
}
