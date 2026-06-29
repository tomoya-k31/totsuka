use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::health::{HealthProbe, MockHealthProbe};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::state::ChildState;
use totsukactl::supervisor::{boot, BootCtx};

fn fake_spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![],
        env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

#[tokio::test]
async fn boot_happy_path_spawns_all_four_in_order() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let spawner_concrete = Arc::new(MockSpawner::default());
    let spawner: Arc<dyn ChildSpawner> = spawner_concrete.clone();
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    for n in [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ] {
        probe_concrete.set_ready(n, true);
    }
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let ctx = BootCtx {
        spawner,
        probe,
        registry: registry.clone(),
        clock,
        paths,
        ready_timeout: Duration::from_secs(5),
    };
    let specs: Vec<_> = [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ]
    .into_iter()
    .map(|n| fake_spec(n, &tmp))
    .collect();

    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) })
        .await
        .unwrap();

    let order = spawner_concrete.spawned.lock().unwrap().clone();
    assert_eq!(order[0], "agent-adapter");
    assert_eq!(order[1], "orchestrator");
    let phase3: std::collections::HashSet<_> = order[2..].iter().cloned().collect();
    assert_eq!(
        phase3,
        ["github-watcher".to_string(), "qa-service".into()]
            .into_iter()
            .collect()
    );
    for n in [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ] {
        assert_eq!(registry.get(n).await.unwrap().state, ChildState::Ready);
    }
}

#[tokio::test]
async fn boot_rolls_back_on_readyz_timeout() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    probe_concrete.set_ready("agent-adapter", false); // never becomes ready
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let ctx = BootCtx {
        spawner,
        probe,
        registry,
        clock,
        paths,
        ready_timeout: Duration::from_millis(200),
    };
    let specs = vec![
        fake_spec("agent-adapter", &tmp),
        fake_spec("orchestrator", &tmp),
        fake_spec("github-watcher", &tmp),
        fake_spec("qa-service", &tmp),
    ];
    let err = boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        totsukactl::error::TotsukactlError::Timeout(_)
    ));
}
