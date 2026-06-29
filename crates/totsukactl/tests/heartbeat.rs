use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_core::SystemClock;
use totsukactl::health::MockHealthProbe;
use totsukactl::heartbeat::{outcome_from, run_healthz_loop, HeartbeatCfg};
use totsukactl::registry::Registry;
use totsukactl::state::{ChildState, HealthOutcome};

#[test]
fn outcome_from_truth_table() {
    assert_eq!(outcome_from(true, true), HealthOutcome::Ok);
    assert_eq!(outcome_from(true, false), HealthOutcome::Degraded);
    assert_eq!(outcome_from(false, true), HealthOutcome::Unhealthy);
    assert_eq!(outcome_from(false, false), HealthOutcome::Unhealthy);
}

#[tokio::test(start_paused = true)]
async fn healthz_loop_transitions_after_unhealthy_threshold() {
    let probe = Arc::new(MockHealthProbe::default());
    probe.set_healthy("orchestrator", false);
    let probe_dyn: Arc<dyn totsukactl::health::HealthProbe> = probe.clone();

    let reg = Arc::new(Registry::new());
    reg.set_state("orchestrator", ChildState::Healthy).await;
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = HeartbeatCfg {
        healthz_interval: Duration::from_secs(5),
        readyz_interval: Duration::from_secs(30),
        pgmq_interval: Duration::from_secs(30),
        degraded_threshold: 2,
        unhealthy_threshold: 3,
    };
    let shutdown = CancellationToken::new();
    let bins = vec!["orchestrator".to_string()];
    let h = {
        let reg = reg.clone();
        let cfg = cfg.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            run_healthz_loop(cfg, probe_dyn, reg, clock, bins, shutdown).await;
        })
    };

    // Advance past the first interval (interval ticks once immediately, then every 5s)
    tokio::time::advance(Duration::from_secs(1)).await; // tick #1 (immediate)
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #2
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #3
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await; // tick #4 — failures>=3 → Unhealthy
    tokio::task::yield_now().await;
    shutdown.cancel();
    h.await.unwrap();

    assert_eq!(reg.get("orchestrator").await.unwrap().state, ChildState::Unhealthy);
}
