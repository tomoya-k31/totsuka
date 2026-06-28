use chrono::Duration;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::error::OrchestratorError;
use crate::sm::Engine;

pub async fn run_timer(
    engine: Arc<Engine>,
    tick_secs: u64,
    shutdown: CancellationToken,
) -> Result<(), OrchestratorError> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let default_to = engine.config.orchestrator.phase_timeout_default_secs as i64;
    let per_phase = engine.config.orchestrator.phase_timeout.clone();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let now = engine.clock.now();
                for phase in ["design", "impl_verify"] {
                    let to = per_phase.get(phase).copied().unwrap_or(default_to as u64) as i64;
                    let deadline = now - Duration::seconds(to);
                    let overdue = engine.repo.list_overdue(deadline, phase).await?;
                    for t in overdue {
                        tracing::warn!(task=%t.id.as_str(), phase, "phase deadline exceeded; marking blocked");
                        let mut updated = t.clone();
                        updated.current_phase = Some(format!("{phase}_blocked"));
                        updated.updated_at = now;
                        let _ = engine.repo.upsert(&updated).await;
                        // Notifier hook deferred to Task 26.
                    }
                }
            }
        }
    }
}
