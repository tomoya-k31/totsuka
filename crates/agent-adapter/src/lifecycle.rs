//! Startup + signal handling. spec §5 (shutdown) and §6 (config reload).

use std::sync::Arc;

use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::herdr::HerdrClient;
use crate::server::AppState;
use totsuka_telemetry::HealthState;

/// One-shot readiness probe. Sets `herdr: ok` if `agent.list` works, else
/// records the failure. Called once at startup and on SIGHUP.
pub async fn probe_ready(herdr: Arc<dyn HerdrClient>, health: &HealthState) {
    match herdr.list().await {
        Ok(_) => health.set_check("herdr", "ok").await,
        Err(e) => health.set_check("herdr", &format!("fail: {e}")).await,
    }
}

/// Verify that every registered repo's `repo_path` exists on disk. Sets
/// `repos_ok: ok` or `repos_ok: fail: <missing>`.
pub async fn probe_repos(state: &AppState) {
    let mut missing = Vec::new();
    for key in state.repos.keys() {
        if let Some(entry) = state.repos.resolve(&key) {
            if !entry.repo_path.exists() {
                missing.push(key.as_str().to_string());
            }
        }
    }
    if missing.is_empty() {
        state.health.set_check("repos_ok", "ok").await;
    } else {
        state
            .health
            .set_check("repos_ok", &format!("fail: missing {missing:?}"))
            .await;
    }
}

/// Block until SIGTERM. On SIGHUP, re-reads config and applies reload.
///
/// On SIGTERM: flip readyz, cancel `shutdown` (the UDS server drains
/// in-flight responses against its own deadline), and return immediately.
/// The supervisor's `shutdown_grace_secs` is a deadline, not a wait we
/// must sit out (spec §5) — sleeping here delays exit past the grace
/// window and triggers 2nd-SIGTERM escalation on every `down`.
pub async fn wait_for_signals(
    state: AppState,
    config_path: String,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = term.recv() => {
                info!("SIGTERM received; initiating graceful shutdown");
                state.health.set_ready(false).await;
                shutdown.cancel();
                return Ok(());
            }
            _ = hup.recv() => {
                info!("SIGHUP received; reloading config");
                match totsuka_config::Config::load(&config_path) {
                    Ok(cfg) => {
                        let report = crate::server::reload::apply_reload(&state, &cfg.agent_adapter);
                        info!(
                            added = report.added.len(),
                            removed = report.removed.len(),
                            "SIGHUP reload applied"
                        );
                    }
                    Err(e) => warn!(error=%e, "SIGHUP reload failed; keeping old config"),
                }
            }
        }
    }
}
