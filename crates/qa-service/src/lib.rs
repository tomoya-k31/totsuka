#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub mod adapter_client;
pub mod classifier;
pub mod error;
pub mod repo_select;
pub mod schema_check;
pub mod thread_map;

pub struct QaApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl QaApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("qa-service stub: nothing to do yet");
        Ok(())
    }
}
