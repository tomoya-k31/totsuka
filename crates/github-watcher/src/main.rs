use std::sync::Arc;

use github_watcher::WatcherApp;
use totsuka_config::Config;
use totsuka_core::SystemClock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(Config::load(&config_path)?);
    tracing_subscriber::fmt().with_env_filter("info").init();
    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    WatcherApp::new(config, clock).run().await
}
