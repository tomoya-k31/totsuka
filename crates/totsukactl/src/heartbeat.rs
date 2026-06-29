use crate::health::HealthProbe;
use crate::pgmq_probe::PgmqProbe;
use crate::registry::Registry;
use crate::state::{next_state, ChildState, HealthOutcome};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_config::schema::HeartbeatSection;
use totsuka_core::Clock;

#[derive(Debug, Clone)]
pub struct HeartbeatCfg {
    pub healthz_interval: Duration,
    pub readyz_interval: Duration,
    pub pgmq_interval: Duration,
    pub degraded_threshold: u32,
    pub unhealthy_threshold: u32,
}

impl From<&HeartbeatSection> for HeartbeatCfg {
    fn from(s: &HeartbeatSection) -> Self {
        Self {
            healthz_interval: Duration::from_secs(s.healthz_interval_secs),
            readyz_interval: Duration::from_secs(s.readyz_interval_secs),
            pgmq_interval: Duration::from_secs(s.pgmq_interval_secs),
            degraded_threshold: s.degraded_threshold,
            unhealthy_threshold: s.unhealthy_threshold,
        }
    }
}

pub fn outcome_from(healthz_ok: bool, readyz_ok: bool) -> HealthOutcome {
    match (healthz_ok, readyz_ok) {
        (true, true) => HealthOutcome::Ok,
        (true, false) => HealthOutcome::Degraded,
        (false, _) => HealthOutcome::Unhealthy,
    }
}

pub async fn run_healthz_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn HealthProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    bins: Vec<String>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.healthz_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                for name in &bins {
                    let ok = probe.healthz(name).await.unwrap_or(false);
                    let now = clock.now();
                    registry.touch_healthz(name, now).await;
                    apply_outcome(&registry, name, ok, /*ready_ok*/ true, &cfg).await;
                }
            }
        }
    }
}

pub async fn run_readyz_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn HealthProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    bins: Vec<String>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.readyz_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                for name in &bins {
                    let ok = probe.readyz(name).await.unwrap_or(false);
                    let now = clock.now();
                    registry.touch_readyz(name, now).await;
                    apply_outcome(&registry, name, /*healthz*/ true, ok, &cfg).await;
                }
            }
        }
    }
}

pub async fn run_pgmq_loop(
    cfg: HeartbeatCfg,
    probe: Arc<dyn PgmqProbe>,
    registry: Arc<Registry>,
    clock: Arc<dyn Clock>,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(cfg.pgmq_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                let ok = probe.ping().await.unwrap_or(false);
                registry.touch_healthz("pgmq", clock.now()).await;
                if ok {
                    registry.reset_failure("pgmq").await;
                    registry.set_state("pgmq", ChildState::Healthy).await;
                } else {
                    let n = registry.bump_failure("pgmq").await;
                    let curr = registry.get("pgmq").await.map(|e| e.state).unwrap_or(ChildState::Healthy);
                    let next = next_state(curr, HealthOutcome::Unhealthy, n, cfg.degraded_threshold, cfg.unhealthy_threshold);
                    registry.set_state("pgmq", next).await;
                }
            }
        }
    }
}

async fn apply_outcome(
    registry: &Registry,
    name: &str,
    healthz_ok: bool,
    readyz_ok: bool,
    cfg: &HeartbeatCfg,
) {
    let outcome = outcome_from(healthz_ok, readyz_ok);
    let curr = registry
        .get(name)
        .await
        .map(|e| e.state)
        .unwrap_or(ChildState::Healthy);
    let next = match outcome {
        HealthOutcome::Ok => {
            registry.reset_failure(name).await;
            next_state(
                curr,
                HealthOutcome::Ok,
                0,
                cfg.degraded_threshold,
                cfg.unhealthy_threshold,
            )
        }
        _ => {
            let n = registry.bump_failure(name).await;
            next_state(
                curr,
                outcome,
                n,
                cfg.degraded_threshold,
                cfg.unhealthy_threshold,
            )
        }
    };
    if next != curr {
        tracing::info!(name, prev=?curr, next=?next, "child state transition");
    }
    registry.set_state(name, next).await;
}
