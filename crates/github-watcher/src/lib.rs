#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub mod column_map;
pub mod error;
pub mod schema_check;

pub struct WatcherApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl WatcherApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("github-watcher stub: nothing to do yet");
        Ok(())
    }
}
