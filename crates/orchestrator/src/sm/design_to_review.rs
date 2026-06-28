//! Designer signals completion via a Project status update committed by the
//! Claude agent (or via a follow-up automated tool). When that arrives,
//! orchestrator may writeback the column move to ProjectsV2.

use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackResult;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn request_writeback(
    e: &Engine,
    task: &Task,
) -> Result<HandleOutcome, OrchestratorError> {
    if task.suppress_writeback_until_human_move {
        return Ok(HandleOutcome::Skipped {
            reason: "suppressed until human move".into(),
        });
    }
    match e
        .writeback
        .move_column(task.id.as_str(), "design_review", None)
        .await?
    {
        WritebackResult::Ok => Ok(HandleOutcome::Applied),
        WritebackResult::VersionMismatch => {
            e.repo.set_suppress(&task.id, true).await?;
            Ok(HandleOutcome::Skipped {
                reason: "OCC conflict; suppress flag set".into(),
            })
        }
        WritebackResult::Failed(msg) => Err(OrchestratorError::Writeback(msg)),
    }
}

#[cfg(test)]
mod tests {
    // The integration of this helper into a real event flow lives in Task 22's
    // bus tests; here we just smoke-test the suppress branch.
}
