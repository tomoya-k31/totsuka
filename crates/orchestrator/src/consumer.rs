use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use totsuka_bus::consumer::Consumer;

use crate::error::OrchestratorError;
use crate::sm::Engine;

pub async fn run_consumer(
    engine: Arc<Engine>,
    pool: PgPool,
    queue: String,
    _batch_size: i32,
    vt_secs: i32,
    shutdown: CancellationToken,
) -> Result<(), OrchestratorError> {
    let consumer = Consumer::new(queue.clone());
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("bus consumer shutting down");
                return Ok(());
            }
            r = consumer.poll_one(&pool, vt_secs) => {
                match r {
                    Ok(Some((msg_id, env))) => {
                        let event_key = env.event_key.clone();
                        // processed_events idempotency
                        if is_processed(&pool, &event_key).await? {
                            consumer.ack(&pool, msg_id).await
                                .map_err(OrchestratorError::Bus)?;
                            continue;
                        }
                        let domain = totsuka_core::DomainEvent {
                            event_key: env.event_key.clone(),
                            source: env.source,
                            event_type: env.event_type.clone(),
                            payload: env.payload.clone(),
                        };
                        match engine.handle(&domain).await {
                            Ok(_) => {
                                mark_processed(&pool, &event_key, &env.event_type, &env.payload).await?;
                                consumer.ack(&pool, msg_id).await
                                    .map_err(OrchestratorError::Bus)?;
                            }
                            Err(err) => {
                                tracing::error!(error=%err, event_key=%event_key, "handler failed; leaving in queue for retry");
                                // don't ack — pgmq visibility timeout will re-deliver
                            }
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(err) => {
                        tracing::error!(error=%err, "consumer poll error; backing off");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }
}

async fn is_processed(pool: &PgPool, key: &str) -> Result<bool, OrchestratorError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT event_key FROM processed_events WHERE event_key = $1 LIMIT 1")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

async fn mark_processed(
    pool: &PgPool,
    key: &str,
    ty: &str,
    payload: &serde_json::Value,
) -> Result<(), OrchestratorError> {
    let hash = format!("{:x}", md5::compute(payload.to_string().as_bytes()));
    sqlx::query(
        "INSERT INTO processed_events (event_key, source, event_type, payload_hash)
         VALUES ($1, 'github', $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(key)
    .bind(ty)
    .bind(hash)
    .execute(pool)
    .await?;
    Ok(())
}
