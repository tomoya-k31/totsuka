//! Filled progressively by Tasks 14-17.

use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};
use totsuka_core::DomainEvent;

pub async fn on_pr_merged_ready(
    _e: &Engine,
    _ev: &DomainEvent,
) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}

pub async fn on_verification(
    _e: &Engine,
    _ev: &DomainEvent,
    _passed: bool,
) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}
