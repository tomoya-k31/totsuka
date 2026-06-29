use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsukactl::child::mock::MockSpawner;
use totsukactl::child::{ChildSpawner, ChildSpec};
use totsukactl::compose::mock::MockCompose;
use totsukactl::compose::ComposeExec;
use totsukactl::health::{HealthProbe, MockHealthProbe};
use totsukactl::heartbeat::{run_healthz_loop, HeartbeatCfg};
use totsukactl::paths::Paths;
use totsukactl::registry::Registry;
use totsukactl::restart::RestartCfg;
use totsukactl::sock_api::{
    bind_uds, router, serve_uds, ControlMsg, SockApiState, SupervisorClient,
};
use totsukactl::state::{ChildState, RestartPolicy};
use totsukactl::supervisor::boot::{boot, BootCtx};
use totsukactl::supervisor::control::handle_restart;
use totsukactl::supervisor::shutdown::{shutdown_stack, ShutdownCfg};

fn spec(name: &str, tmp: &TempDir) -> ChildSpec {
    ChildSpec {
        name: name.into(),
        bin_path: tmp.path().join(name),
        args: vec![],
        env: vec![],
        log_path: tmp.path().join(format!("{name}.log")),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_boot_status_restart_down() {
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    paths.ensure().unwrap();
    let compose: Arc<dyn ComposeExec> =
        Arc::new(MockCompose::with_image("ghcr.io/pgmq/pg18-pgmq:v1.11.1"));
    let spawner: Arc<dyn ChildSpawner> = Arc::new(MockSpawner::default());
    let probe_concrete = Arc::new(MockHealthProbe::default());
    let probe: Arc<dyn HealthProbe> = probe_concrete.clone();
    for n in [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ] {
        probe_concrete.set_ready(n, true);
        probe_concrete.set_healthy(n, true);
    }
    let registry = Arc::new(Registry::new());
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);

    let ctx = BootCtx {
        spawner: spawner.clone(),
        probe: probe.clone(),
        registry: registry.clone(),
        clock: clock.clone(),
        paths: paths.clone(),
        ready_timeout: Duration::from_secs(2),
    };
    let specs: Vec<_> = [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ]
    .into_iter()
    .map(|n| spec(n, &tmp))
    .collect();
    boot(&ctx, &specs, async { Ok(()) }, async { Ok(()) })
        .await
        .unwrap();

    // Spawn sock_api server
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<ControlMsg>(8);
    let listener = bind_uds(&paths.supervisor_sock()).await.unwrap();
    let state = SockApiState {
        registry: registry.clone(),
        control_tx: ctl_tx,
    };
    let r = router(state);
    let _h_sock = tokio::spawn(async move {
        let _ = serve_uds(listener, r).await;
    });

    // Spawn healthz ticker
    let cancel = CancellationToken::new();
    let hb = HeartbeatCfg {
        healthz_interval: Duration::from_millis(50),
        readyz_interval: Duration::from_secs(30),
        pgmq_interval: Duration::from_secs(30),
        degraded_threshold: 2,
        unhealthy_threshold: 3,
    };
    let bins = vec![
        "agent-adapter".into(),
        "orchestrator".into(),
        "github-watcher".into(),
        "qa-service".into(),
    ];
    let _h_hb = tokio::spawn(run_healthz_loop(
        hb,
        probe.clone(),
        registry.clone(),
        clock.clone(),
        bins,
        cancel.clone(),
    ));

    // Status via supervisor client
    let client = SupervisorClient::new(paths.supervisor_sock());
    let list = client.list().await.unwrap();
    assert!(list.iter().any(|p| p.name == "orchestrator"));

    // Drive a restart via UDS
    client.restart("orchestrator").await.unwrap();
    let msg = ctl_rx.recv().await.unwrap();
    if let ControlMsg::Restart(name) = msg {
        let rcfg = RestartCfg {
            policy: RestartPolicy::OnDeadOnly,
            backoff_secs: vec![0],
            max_attempts: 3,
        };
        handle_restart(
            &name,
            registry.clone(),
            spawner.clone(),
            &specs,
            &paths,
            clock.clone(),
            &rcfg,
            Duration::from_millis(10),
        )
        .await
        .unwrap();
    }
    assert_eq!(registry.get("orchestrator").await.unwrap().restart_count, 1);

    // Drive shutdown
    cancel.cancel();
    shutdown_stack(
        ShutdownCfg {
            grace: Duration::from_millis(20),
            second_term: Duration::from_millis(10),
            force_grace: Duration::from_millis(20),
            also_postgres: false,
            force: true,
        },
        registry.clone(),
        compose.clone(),
        paths.clone(),
    )
    .await
    .unwrap();
    for n in [
        "agent-adapter",
        "orchestrator",
        "github-watcher",
        "qa-service",
    ] {
        assert_eq!(registry.get(n).await.unwrap().state, ChildState::Stopped);
    }
}
