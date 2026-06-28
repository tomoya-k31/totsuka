//! Final review is a human gate (parent §4.1). orchestrator just records the
//! column move; WIP is naturally bounded because each task occupies its slot
//! until either AwaitingRelease or rejected back to ImplVerify.

use crate::error::OrchestratorError;
use crate::repository::Task;
use crate::sm::{Engine, HandleOutcome};

pub async fn on_enter(_e: &Engine, _task: &Task) -> Result<HandleOutcome, OrchestratorError> {
    tracing::info!(task=%_task.id.as_str(), "task entered final_review (human gate)");
    Ok(HandleOutcome::Applied)
}
