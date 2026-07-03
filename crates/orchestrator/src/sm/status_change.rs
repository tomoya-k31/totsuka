use serde::Deserialize;
use totsuka_core::{DomainEvent, TaskId};

use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct StatusChanged {
    pub item_id: String,
    pub to_status: String,
    #[serde(default)]
    pub repo: String,
    /// GitHub issue number behind the project item (absent for drafts and
    /// for events emitted before the watcher carried it).
    #[serde(default)]
    pub issue_number: Option<i64>,
}

pub async fn handle(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: StatusChanged = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload parse: {err}")))?;
    let id = TaskId::new(p.item_id.clone());
    upsert_column(e, &p).await?;
    // The 🤖 columns are the agent triggers: a human moving a card into
    // 調査・設計 (design) or 実装・受入検証 (impl_verify) starts the
    // corresponding agent. 📋 Ready is just a backlog state.
    if p.to_status == "design" {
        if let Some(t) = e.repo.get(&id).await? {
            return super::ready_to_design::try_spawn(e, &t).await;
        }
    }
    if p.to_status == "impl_verify" {
        if let Some(t) = e.repo.get(&id).await? {
            return super::impl_verify::on_enter(e, &t).await;
        }
    }
    if p.to_status == "final_review" {
        if let Some(t) = e.repo.get(&id).await? {
            return super::final_review::on_enter(e, &t).await;
        }
    }
    Ok(HandleOutcome::Applied)
}

pub async fn on_human_gate(
    e: &Engine,
    ev: &DomainEvent,
) -> Result<HandleOutcome, OrchestratorError> {
    let p: StatusChanged = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload parse: {err}")))?;
    let id = TaskId::new(p.item_id.clone());
    e.repo.set_suppress(&id, false).await?;
    upsert_column(e, &p).await?;
    Ok(HandleOutcome::Applied)
}

async fn upsert_column(e: &Engine, p: &StatusChanged) -> Result<(), OrchestratorError> {
    let id = TaskId::new(p.item_id.clone());
    let now = e.clock.now();
    let existing = e.repo.get(&id).await?;
    let task = match existing {
        Some(mut t) => {
            t.current_column = p.to_status.clone();
            // Don't clobber a known number with None (older events).
            if p.issue_number.is_some() {
                t.issue_number = p.issue_number;
            }
            t.updated_at = now;
            t
        }
        None => Task {
            id: id.clone(),
            task_id_short: id.short(),
            repo: p.repo.clone(),
            issue_number: p.issue_number,
            pr_node_id: None,
            current_column: p.to_status.clone(),
            current_phase: None,
            impl_verify_attempt: 0,
            suppress_writeback_until_human_move: false,
            spawned_at: None,
            created_at: now,
            updated_at: now,
        },
    };
    e.repo.upsert(&task).await?;
    Ok(())
}
