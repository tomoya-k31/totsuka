#![forbid(unsafe_code)]

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

pub mod adapter_client;
pub mod answer;
pub mod catchup;
pub mod classifier;
pub mod error;
pub mod gh_inbox;
pub mod lifecycle;
pub mod listener;
pub mod mode;
pub mod question_filter;
pub mod reaction;
pub mod recovery;
pub mod repo_select;
pub mod schema_check;
pub mod slack;
pub mod sweeper;
pub mod thread_history;
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
