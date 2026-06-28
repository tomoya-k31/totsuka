//! On startup, reconcile qa_thread_agent vs agent-adapter's agent.list:
//! * mapping ∧ agent → keep
//! * mapping ∧ ¬agent → DELETE mapping (next thread message will spawn fresh)
//! * ¬mapping ∧ agent (qa-labelled) → close pane (avoid leak)
//!
//! See spec §8.4 「再起動時のリカバリ」.

use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use crate::thread_map::ThreadMapRepo;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub kept: usize,
    pub mapping_orphans_deleted: usize,
    pub pane_orphans_closed: usize,
}

pub async fn reconcile(
    thread_map: &ThreadMapRepo,
    adapter: &dyn AdapterClient,
) -> Result<RecoveryReport, QaError> {
    // TODO: agent-adapter's GET /v1/agents endpoint is not yet implemented.
    // This function assumes AdapterClient::list() works. The adapter side
    // handler should be added to crates/agent-adapter/src/server/list.rs.
    let agents = adapter.list().await?;
    let mappings = thread_map.list_all().await?;

    let alive: HashSet<String> = agents.iter().map(|a| a.terminal_id.clone()).collect();
    let mut kept = 0usize;
    let mut mapping_orphans_deleted = 0usize;

    let mapped: HashSet<String> = mappings.iter().map(|m| m.terminal_id.clone()).collect();

    for m in &mappings {
        if alive.contains(&m.terminal_id) {
            kept += 1;
        } else {
            thread_map.delete(&m.thread_ts).await?;
            mapping_orphans_deleted += 1;
        }
    }

    let mut pane_orphans_closed = 0usize;
    for a in &agents {
        if !a.label.starts_with("totsuka:qa-") {
            // Not a qa-service agent — leave it alone.
            continue;
        }
        if mapped.contains(&a.terminal_id) {
            continue;
        }
        if let Err(e) = adapter.stop(&a.agent_id, "", "").await {
            tracing::warn!(error=%e, agent_id=%a.agent_id, "recovery: pane orphan close failed");
            continue;
        }
        pane_orphans_closed += 1;
    }

    tracing::info!(
        kept,
        mapping_orphans_deleted,
        pane_orphans_closed,
        "qa-service recovery complete"
    );
    Ok(RecoveryReport {
        kept,
        mapping_orphans_deleted,
        pane_orphans_closed,
    })
}
