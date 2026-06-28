//! Filled progressively (Tasks 12+18 wire concrete transitions through it).

use crate::error::OrchestratorError;
use crate::sm::{Engine, HandleOutcome};
use totsuka_core::DomainEvent;

pub async fn handle(_e: &Engine, _ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}

pub async fn on_human_gate(
    _e: &Engine,
    _ev: &DomainEvent,
) -> Result<HandleOutcome, OrchestratorError> {
    Ok(HandleOutcome::Skipped {
        reason: "not yet implemented".into(),
    })
}
