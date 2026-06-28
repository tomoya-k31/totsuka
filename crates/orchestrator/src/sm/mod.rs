use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::{Clock, DomainEvent};

use crate::adapter_client::AdapterClient;
use crate::effect::EffectLedger;
use crate::error::OrchestratorError;
use crate::gh_writeback::WritebackClient;
use crate::repository::Repository;
use crate::wip::WipGate;

pub struct Engine {
    pub repo: Arc<dyn Repository>,
    pub adapter: Arc<dyn AdapterClient>,
    pub writeback: Arc<dyn WritebackClient>,
    pub effects: Arc<EffectLedger>,
    pub wip: Arc<WipGate>,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<Config>,
    pub owner_id: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandleOutcome {
    Applied,
    Skipped { reason: String },
    WipFull,
}

impl Engine {
    pub async fn handle(&self, ev: &DomainEvent) -> Result<HandleOutcome, OrchestratorError> {
        match ev.event_type.as_str() {
            "github.status_changed" => status_change::handle(self, ev).await,
            "github.pr_merged_ready" => impl_verify::on_pr_merged_ready(self, ev).await,
            "github.pr_verification_passed" => impl_verify::on_verification(self, ev, true).await,
            "github.pr_verification_failed" => impl_verify::on_verification(self, ev, false).await,
            "github.release_published" => released::handle(self, ev).await,
            "human.gate_passed" => status_change::on_human_gate(self, ev).await,
            other => {
                tracing::debug!(ty=%other, "unhandled event type");
                Ok(HandleOutcome::Skipped {
                    reason: format!("unhandled: {other}"),
                })
            }
        }
    }
}

// Module declarations — each fills its file in subsequent tasks.
pub mod design_to_review;
pub mod final_review;
pub mod impl_verify;
pub mod ready_to_design;
pub mod released;
pub mod status_change;
