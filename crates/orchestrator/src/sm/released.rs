use serde::Deserialize;
use totsuka_core::DomainEvent;

use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::sm::{Engine, HandleOutcome};

#[derive(Deserialize)]
struct ReleasePublished {
    pub repo: String,
}

pub async fn handle(e: &Engine, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    let p: ReleasePublished = serde_json::from_value(ev.payload.clone())
        .map_err(|err| OrchestratorError::Internal(format!("payload: {err}")))?;
    let tasks = e.repo.list_awaiting_release_in_repo(&p.repo).await?;
    let mut applied = 0;
    for t in tasks {
        if t.suppress_writeback_until_human_move {
            continue;
        }
        match e
            .writeback
            .move_column(t.id.as_str(), "released", None)
            .await?
        {
            WritebackResult::Ok => applied += 1,
            WritebackResult::VersionMismatch => {
                e.repo.set_suppress(&t.id, true).await?;
            }
            WritebackResult::Failed(msg) => {
                tracing::warn!(task_id=%t.id, error=%msg, "writeback failed for awaiting_release task");
            }
        }
    }
    tracing::info!(repo=%p.repo, applied, "released event processed");
    Ok(HandleOutcome::Applied)
}
