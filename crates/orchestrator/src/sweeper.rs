use crate::error::OrchestratorError;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

pub async fn run_sweeper(
    pool: PgPool,
    tick_secs: u64,
    shutdown: CancellationToken,
) -> Result<(), OrchestratorError> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                let row = sqlx::query(
                    "UPDATE processed_effects SET status = 'pending',
                     lease_owner = NULL, lease_expires_at = NULL, updated_at = now()
                     WHERE status = 'in_progress' AND lease_expires_at <= now()"
                ).execute(&pool).await
                .map_err(OrchestratorError::Sqlx)?;
                if row.rows_affected() > 0 {
                    tracing::info!(recovered = row.rows_affected(), "sweeper recovered expired leases");
                }
            }
        }
    }
}
