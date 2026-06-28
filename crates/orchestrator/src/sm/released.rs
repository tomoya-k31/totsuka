//! Filled by Task 19.

use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};
use totsuka_core::DomainEvent;

pub async fn handle(_e: &Engine, _ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}
