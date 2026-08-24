//! [`poll_loop`]: the fetch→submit timer for polling-style sources (GitHub,
//! Notion) that migrated off the deprecated `tasks/fetch`.
//!
//! Each tick fetches every workflow's trigger (the
//! `InitializeParams.workflows` supplied at initialize) and submits each task
//! **naming that workflow** (0.6.0, #554) — the loop already iterates per
//! workflow, so the answer the Orchestrator used to re-derive is right here. Ticks never overlap — the
//! next sleep starts after the current pass finishes — and the interval is
//! jittered ±10% so multiple plugins do not thundering-herd a shared API.
//! Duplicate submissions are cheap `duplicate` acks, so no seen-set is kept.

use std::time::Duration;

use plugin_protocol::Task;
use plugin_protocol::methods::WorkflowInfo;

use crate::submit::{SubmitOutcome, Submitter};

/// Run the fetch→submit loop forever (spawn it; it ends only with the task).
///
/// `fetch` returns the tasks currently matching one trigger, or a
/// human-readable error — a failing trigger skips this tick, not the loop
/// (transient API failures must not kill the source).
pub async fn poll_loop<S, F, Fut>(
    triggers: Vec<WorkflowInfo>,
    interval: Duration,
    submitter: S,
    mut fetch: F,
) where
    S: Submitter,
    F: FnMut(&WorkflowInfo) -> Fut + Send,
    Fut: Future<Output = Result<Vec<Task>, String>> + Send,
{
    let mut tick: u64 = 0;
    loop {
        for trigger in &triggers {
            let tasks = match fetch(trigger).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::warn!(
                        workflow = %trigger.workflow,
                        "fetch failed (skipping this tick): {e}"
                    );
                    continue;
                }
            };
            for task in tasks {
                let task_id = task.id.clone();
                match submitter.submit(task, &trigger.workflow).await {
                    SubmitOutcome::Accepted => {
                        tracing::info!(task = %task_id, workflow = %trigger.workflow, "task submitted");
                    }
                    // The normal steady state: the task was already ingested
                    // on an earlier tick.
                    SubmitOutcome::Duplicate => {}
                    SubmitOutcome::Rejected { reason } => {
                        tracing::warn!(
                            task = %task_id,
                            workflow = %trigger.workflow,
                            "task rejected: {}",
                            reason.as_deref().unwrap_or("no reason given")
                        );
                    }
                    SubmitOutcome::GaveUp { error } => {
                        tracing::error!(
                            task = %task_id,
                            workflow = %trigger.workflow,
                            "task submission gave up: {error} → will retry on a later tick"
                        );
                    }
                }
            }
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(jittered(interval, tick)).await;
    }
}

/// `interval` ±10%, varied deterministically per tick (no rand dependency —
/// decorrelation, not cryptography).
fn jittered(interval: Duration, tick: u64) -> Duration {
    let base = interval.as_millis() as u64;
    if base == 0 {
        return interval;
    }
    let spread = base / 5; // ±10% → a 20% window
    if spread == 0 {
        return interval;
    }
    // Cheap deterministic scatter: SplitMix64 of the tick counter.
    let mut z = tick.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let offset = z % spread;
    Duration::from_millis(base - spread / 2 + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_ten_percent() {
        let interval = Duration::from_secs(60);
        for tick in 0..1000 {
            let j = jittered(interval, tick);
            assert!(j >= Duration::from_secs(54), "{j:?}");
            assert!(j <= Duration::from_secs(66), "{j:?}");
        }
    }

    #[test]
    fn tiny_intervals_pass_through() {
        assert_eq!(jittered(Duration::ZERO, 3), Duration::ZERO);
        assert_eq!(
            jittered(Duration::from_millis(4), 3),
            Duration::from_millis(4)
        );
    }
}
