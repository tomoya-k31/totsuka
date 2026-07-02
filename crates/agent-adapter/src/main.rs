use std::sync::Arc;
use std::time::Duration;

use agent_adapter::{
    gc::spawn_gc_loop,
    herdr::wire::WireHerdr,
    lifecycle::{probe_ready, probe_repos, wait_for_signals},
    listener::{bind_uds, serve_uds},
    repo::RepoRegistry,
    server::{router, AppState},
    worktree::WorktreeManager,
};
use totsuka_config::resolve_tilde;
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;

/// Upper bound on the post-SIGTERM in-flight drain. Must stay well under
/// `supervisor.shutdown_grace_secs` (default 15s) or every `down` escalates
/// to a 2nd SIGTERM. spec §5: adapter HTTP responses return immediately.
const SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let config = Arc::new(totsuka_config::Config::load(&config_path)?);

    let state_dir = std::path::PathBuf::from(&config.totsuka.state_dir);
    let _log_guard =
        totsuka_telemetry::init_tracing(&state_dir, "agent-adapter", &config.totsuka.log_level);

    let herdr_socket = std::path::PathBuf::from(resolve_tilde(
        &config.agent_adapter.herdr_socket,
        std::env::var("HOME").ok().as_deref(),
    ));
    let herdr: Arc<dyn agent_adapter::herdr::HerdrClient> =
        Arc::new(WireHerdr::connect(&herdr_socket).await?);

    let repos = Arc::new(RepoRegistry::new());
    repos.reload(&config.agent_adapter);

    let health = HealthState::new();
    let state = AppState {
        herdr: herdr.clone(),
        repos: repos.clone(),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: health.clone(),
    };

    probe_ready(state.herdr.clone(), &state.health).await;
    probe_repos(&state).await;
    health.set_ready(true).await;

    let gc_interval = Duration::from_secs(config.agent_adapter.worktree_orphan_scan_interval_secs);
    let _gc = spawn_gc_loop(state.clone(), gc_interval);

    let uds = std::path::PathBuf::from(resolve_tilde(
        &config.agent_adapter.uds_path,
        std::env::var("HOME").ok().as_deref(),
    ));
    let listener = bind_uds(&uds).await?;
    tracing::info!(path=?uds, "agent-adapter listening on UDS");

    let app = router(state.clone());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut server = tokio::spawn(serve_uds(
        listener,
        app,
        shutdown.clone(),
        SHUTDOWN_DRAIN_DEADLINE,
    ));
    let signals = tokio::spawn(wait_for_signals(
        state.clone(),
        config_path,
        shutdown.clone(),
    ));

    tokio::select! {
        r = &mut server => { r??; },
        r = signals => {
            r??;
            // SIGTERM path: the token is cancelled; wait for the server to
            // finish draining (bounded by SHUTDOWN_DRAIN_DEADLINE inside
            // serve_uds), then exit.
            server.await??;
        },
    }

    Ok(())
}
