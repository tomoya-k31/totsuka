use crate::child::{ChildSpawner, ChildSpec};
use crate::paths::Paths;
use crate::registry::Registry;
use crate::restart::{decide, RestartCfg, RestartDecision};
use crate::supervisor::control::handle_restart;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_core::Clock;

/// Periodically inspect the registry for Dead/Unhealthy children and respawn
/// per restart_policy. pgmq is always skipped (spec §7 no-cascade-restart).
#[allow(clippy::too_many_arguments)]
pub async fn run_restart_tick(
    interval: Duration,
    registry: Arc<Registry>,
    spawner: Arc<dyn ChildSpawner>,
    specs: Vec<ChildSpec>,
    paths: Paths,
    clock: Arc<dyn Clock>,
    restart_cfg: RestartCfg,
    kill_grace: Duration,
    shutdown: CancellationToken,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tick.tick() => {
                for entry in registry.list().await {
                    if entry.name == "pgmq" { continue; }
                    let decision = decide(entry.state, entry.restart_count, &restart_cfg);
                    let backoff = match decision {
                        RestartDecision::Wait(b) => b,
                        RestartDecision::Skip | RestartDecision::GiveUp => continue,
                    };
                    let now = clock.now();
                    let elapsed_ok = entry.last_restart_attempt_at
                        .map(|t| (now - t).to_std().map(|d| d >= backoff).unwrap_or(true))
                        .unwrap_or(true);
                    if !elapsed_ok { continue; }
                    registry.touch_restart_attempt(&entry.name, now).await;
                    tracing::info!(child=%entry.name, ?backoff, "auto-restart triggered by restart_policy");
                    if let Err(e) = handle_restart(
                        &entry.name,
                        registry.clone(),
                        spawner.clone(),
                        &specs,
                        &paths,
                        clock.clone(),
                        &restart_cfg,
                        kill_grace,
                    ).await {
                        tracing::error!(child=%entry.name, error=%e, "auto-restart failed");
                    }
                }
            }
        }
    }
}
