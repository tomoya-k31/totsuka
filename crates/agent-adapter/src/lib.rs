#![forbid(unsafe_code)]

pub mod argv;
pub mod error;
pub mod herdr;
pub mod repo;
pub mod server;
pub mod worktree;

use std::sync::Arc;
use totsuka_config::Config;
use totsuka_core::Clock;

/// Top-level wiring for the agent-adapter binary. Holds shared dependencies
/// constructed once at startup; `run()` blocks until SIGTERM.
pub struct AdapterApp {
    #[allow(dead_code)]
    config: Arc<Config>,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl AdapterApp {
    pub fn new(config: Arc<Config>, clock: Arc<dyn Clock>) -> Self {
        Self { config, clock }
    }

    /// Stub. Later tasks replace this with the full lifecycle.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!("agent-adapter stub: nothing to do yet");
        Ok(())
    }
}
