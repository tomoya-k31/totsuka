use serde_json::Value;
use sqlx::{PgPool, Row};

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, BusError>;

/// Create a pgmq queue. Idempotent: silently succeeds if the queue already exists.
pub async fn create_queue(pool: &PgPool, name: &str) -> Result<()> {
    sqlx::query("SELECT pgmq.create($1)")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// Send a JSON payload to the named queue. Returns the assigned message ID.
pub async fn send_json(pool: &PgPool, name: &str, payload: &Value) -> Result<i64> {
    let row = sqlx::query("SELECT pgmq.send($1, $2::jsonb) AS msg_id")
        .bind(name)
        .bind(payload)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("msg_id"))
}

/// A message returned by pgmq.
#[derive(Debug, Clone)]
pub struct PgmqMessage {
    pub msg_id: i64,
    pub read_ct: i32,
    pub message: Value,
}

/// Read up to one message, locking it for `vt_secs` seconds (visibility timeout).
pub async fn read_one(pool: &PgPool, name: &str, vt_secs: i32) -> Result<Option<PgmqMessage>> {
    // pgmq.read(queue_name, vt, qty) returns SETOF pgmq.message_record
    let rows = sqlx::query("SELECT msg_id, read_ct, message FROM pgmq.read($1, $2, 1)")
        .bind(name)
        .bind(vt_secs)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().next().map(|r| PgmqMessage {
        msg_id: r.get("msg_id"),
        read_ct: r.get("read_ct"),
        message: r.get("message"),
    }))
}

/// Acknowledge (delete) a message by ID. Returns `true` if the message was found and deleted.
pub async fn delete(pool: &PgPool, name: &str, msg_id: i64) -> Result<bool> {
    let row = sqlx::query("SELECT pgmq.delete($1, $2) AS ok")
        .bind(name)
        .bind(msg_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<bool, _>("ok"))
}
