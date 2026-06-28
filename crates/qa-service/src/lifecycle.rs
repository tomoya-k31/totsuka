use crate::adapter_client::AdapterClient;
use crate::error::QaError;
use sqlx::PgPool;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;
use totsuka_config::Config;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_adapter(adapter: &dyn AdapterClient, health: &HealthState) {
    match adapter.list().await {
        Ok(_) => health.set_check("adapter", "ok").await,
        Err(e) => health.set_check("adapter", &format!("fail: {e}")).await,
    }
}

pub async fn probe_repo_descriptions(config: &Config, health: &HealthState) {
    let missing: Vec<&String> = config
        .agent_adapter
        .repos
        .iter()
        .filter(|(_, r)| r.description.trim().is_empty())
        .map(|(name, _)| name)
        .collect();
    if missing.is_empty() {
        health.set_check("repo_descriptions", "ok").await;
    } else {
        let msg = format!("fail: empty description for: {:?}", missing);
        health.set_check("repo_descriptions", &msg).await;
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), QaError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| QaError::Internal(format!("install SIGTERM: {e}")))?;
    let mut int = signal(SignalKind::interrupt())
        .map_err(|e| QaError::Internal(format!("install SIGINT: {e}")))?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received; initiating graceful shutdown"),
        _ = int.recv()  => tracing::info!("SIGINT received; initiating graceful shutdown"),
    }
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
