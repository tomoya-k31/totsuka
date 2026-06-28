use sqlx::PgPool;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;

use crate::adapter_client::AdapterClient;
use crate::error::OrchestratorError;
use totsuka_telemetry::HealthState;

pub async fn probe_db(pool: &PgPool, health: &HealthState) {
    match sqlx::query("SELECT 1").execute(pool).await {
        Ok(_) => health.set_check("db", "ok").await,
        Err(e) => health.set_check("db", &format!("fail: {e}")).await,
    }
}

pub async fn probe_adapter(adapter: Arc<dyn AdapterClient>, health: &HealthState) {
    let r = adapter.read("__probe__", 0).await;
    match r {
        Ok(_) => health.set_check("adapter", "ok").await,
        Err(e) => {
            let s = e.to_string();
            if s.contains("not_found") || s.contains("not found") {
                health.set_check("adapter", "ok").await;
            } else {
                health.set_check("adapter", &format!("fail: {e}")).await;
            }
        }
    }
}

pub async fn wait_for_signals(shutdown: CancellationToken) -> Result<(), OrchestratorError> {
    let mut term = signal(SignalKind::terminate())
        .map_err(|e| OrchestratorError::Internal(format!("install SIGTERM handler: {e}")))?;
    let mut int = signal(SignalKind::interrupt())
        .map_err(|e| OrchestratorError::Internal(format!("install SIGINT handler: {e}")))?;
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received; initiating graceful shutdown"),
        _ = int.recv()  => tracing::info!("SIGINT received; initiating graceful shutdown"),
    }
    shutdown.cancel();
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    Ok(())
}
