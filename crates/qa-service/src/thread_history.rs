//! Persisted QA conversation history (`qa_thread_history`). Delegated-mode
//! answers are ephemeral and never land in the Slack thread, so this table
//! is the only source for restoring conversation context when a thread's
//! pane has already been swept and a fresh agent is spawned.

use crate::error::QaError;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use totsuka_core::Clock;

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub role: String, // "user" | "assistant"
    pub body: String,
}

pub struct ThreadHistoryRepo {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl ThreadHistoryRepo {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    pub async fn append(&self, thread_ts: &str, role: &str, body: &str) -> Result<(), QaError> {
        sqlx::query(
            "INSERT INTO qa_thread_history (thread_ts, role, body, created_at)
                  VALUES ($1, $2, $3, $4)",
        )
        .bind(thread_ts)
        .bind(role)
        .bind(body)
        .bind(self.clock.now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Last `limit` entries in chronological order.
    pub async fn recent(&self, thread_ts: &str, limit: i64) -> Result<Vec<HistoryEntry>, QaError> {
        let rows = sqlx::query(
            "SELECT role, body FROM (
                    SELECT id, role, body FROM qa_thread_history
                     WHERE thread_ts = $1 ORDER BY id DESC LIMIT $2
                  ) t ORDER BY id ASC",
        )
        .bind(thread_ts)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| HistoryEntry {
                role: r.get("role"),
                body: r.get("body"),
            })
            .collect())
    }
}
