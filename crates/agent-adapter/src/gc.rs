//! Orphan-worktree scanner. spec §11.16: periodically diff on-disk worktrees
//! against live herdr panes; remove orphans that no agent owns. Failed
//! worktrees are retained for `worktree_failed_ttl_hours` (handled by callers
//! that mark the directory mtime; this module is a pure set-difference).

use std::collections::HashSet;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::server::AppState;

#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub total: usize,
    pub removed: usize,
    pub kept: usize,
}

/// Extract the task-id segment from a herdr label of the form
/// `totsuka:<task>:<phase>:<attempt>`.
/// Returns `None` for non-totsuka labels (panes managed by something else).
fn label_to_branch(label: &str) -> Option<String> {
    // We can't reconstruct the exact branch from the label alone; instead we
    // rely on the branch being present in `git worktree list`'s record AND
    // some live label matching the task_id substring. Simpler heuristic: keep
    // any worktree whose branch contains a `task_id_short` (12 hex) for which
    // a live label exists.
    label
        .strip_prefix("totsuka:")
        .and_then(|rest| rest.split(':').next())
        .map(|task_id| task_id.to_string())
}

pub async fn gc_tick(state: &AppState) -> GcReport {
    let mut report = GcReport::default();

    let live_task_ids: HashSet<String> = match state.herdr.list().await {
        Ok(items) => items
            .iter()
            .filter_map(|i| label_to_branch(&i.label))
            .collect(),
        Err(e) => {
            warn!(error=%e, "gc: herdr.list failed; skipping tick");
            return report;
        }
    };

    // Iterate every registered repo. `RepoRegistry::keys` snapshots the loaded
    // keys so there is no lock held during the async worktree operations below.
    for key in state.repos.keys() {
        let entry = match state.repos.resolve(&key) {
            Some(e) => e,
            None => continue,
        };
        let records = match state.worktrees.list(&entry).await {
            Ok(r) => r,
            Err(e) => {
                warn!(repo=%key.as_str(), error=%e, "gc: worktree list failed");
                continue;
            }
        };
        report.total += records.len();

        for rec in records {
            let Some(branch) = rec.branch.as_deref() else {
                report.kept += 1;
                continue;
            };
            // Branch shape: totsuka/<task_id_short>/<phase_short>
            let task_id_short = branch
                .strip_prefix("totsuka/")
                .and_then(|rest| rest.split('/').next());
            let is_live = task_id_short
                .map(|s| live_task_ids.iter().any(|t| t.ends_with(s)))
                .unwrap_or(false);
            if is_live {
                report.kept += 1;
            } else if let Err(e) = state.worktrees.remove(&entry, branch).await {
                warn!(branch=%branch, error=%e, "gc: remove failed");
                report.kept += 1;
            } else {
                info!(branch=%branch, "gc: removed orphan worktree");
                report.removed += 1;
            }
        }
    }
    report
}

pub fn spawn_gc_loop(state: AppState, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let report = gc_tick(&state).await;
            info!(
                total = report.total,
                removed = report.removed,
                kept = report.kept,
                "worktree gc tick"
            );
        }
    })
}
