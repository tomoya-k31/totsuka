use crate::error::WatcherError;
use crate::gh_client::GhClient;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_)  => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_github(
    client: &Arc<dyn GhClient>,
    owner: &str,
    number: u64,
    health: &HealthState,
) {
    match client.resolve_project_node_id(owner, number).await {
        Ok(_)  => health.set_check("github", "ok").await,
        Err(e) => health.set_check("github", &format!("fail: {e}")).await,
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), WatcherError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| WatcherError::Internal(format!("install SIGTERM: {e}")))?;
    let mut int = signal(SignalKind::interrupt())
        .map_err(|e| WatcherError::Internal(format!("install SIGINT: {e}")))?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received; initiating graceful shutdown"),
        _ = int.recv()  => tracing::info!("SIGINT received; initiating graceful shutdown"),
    }
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
