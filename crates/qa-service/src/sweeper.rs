//! Idle pane sweeper. spec §8.4 — close panes whose thread has been silent
//! for [qa_service.answer].pane_idle_ttl_secs.

use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use crate::thread_map::ThreadMapRepo;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use totsuka_core::Clock;

pub async fn run_sweeper(
    thread_map: Arc<ThreadMapRepo>,
    adapter: Arc<dyn AdapterClient>,
    clock: Arc<dyn Clock>,
    idle_ttl: chrono::Duration,
    tick_secs: u64,
    shutdown: CancellationToken,
) -> Result<(), QaError> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let cutoff = clock.now() - idle_ttl;
                let idle = match thread_map.list_idle(cutoff).await {
                    Ok(v) => v,
                    Err(e) => { tracing::warn!(error=%e, "sweeper list_idle failed"); continue; }
                };
                for m in idle {
                    let branch = format!("qa/{}", sanitize(&m.thread_ts));
                    if let Err(e) = adapter.stop(&m.terminal_id, &m.repo, &branch).await {
                        // A pane that is already gone must still lose its
                        // mapping, or the row leaks forever and later
                        // continuations target a dead terminal. Keep the
                        // mapping only on errors that may be transient.
                        if !crate::adapter_client::is_agent_gone(&e) {
                            tracing::warn!(error=%e, thread_ts=%m.thread_ts, "sweeper stop failed");
                            continue;
                        }
                        tracing::warn!(error=%e, thread_ts=%m.thread_ts, terminal_id=%m.terminal_id,
                            "pane already gone; dropping stale mapping");
                    }
                    if let Err(e) = thread_map.delete(&m.thread_ts).await {
                        tracing::warn!(error=%e, thread_ts=%m.thread_ts, "sweeper delete failed");
                    }
                }
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}
