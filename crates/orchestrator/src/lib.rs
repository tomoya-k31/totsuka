#![forbid(unsafe_code)]

pub mod error;
pub mod schema_check;

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
