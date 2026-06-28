#![forbid(unsafe_code)]

pub mod adapter_client;
pub mod argv;
pub mod branch;
pub mod consumer;
pub mod conversation;
pub mod effect;
pub mod error;
pub mod gh_writeback;
pub mod lifecycle;
pub mod repository;
pub mod schema_check;
pub mod sm;
pub mod sweeper;
pub mod timer;
pub mod wip;

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub struct OrchestratorApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl OrchestratorApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("orchestrator stub: nothing to do yet");
        Ok(())
    }
}
