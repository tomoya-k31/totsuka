//! spec §8.4 — qa_thread_agent table: maps Slack thread_ts → herdr terminal_id.

use crate::error::QaError;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use totsuka_core::Clock;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMapping {
    pub thread_ts: String,
    pub terminal_id: String,
    pub repo: String,
    /// スレッドの由来。'owner' | 'self_mention'。self_mention 由来は owner の
    /// 素の返信では継続発火しない(question_filter::evaluate 参照)。
    pub origin: String,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

pub struct ThreadMapRepo {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl ThreadMapRepo {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    pub async fn get(&self, thread_ts: &str) -> Result<Option<ThreadMapping>, QaError> {
        let row = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, origin, last_activity_at, created_at
               FROM qa_thread_agent WHERE thread_ts = $1",
        )
        .bind(thread_ts)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ThreadMapping {
            thread_ts: r.get("thread_ts"),
            terminal_id: r.get("terminal_id"),
            repo: r.get("repo"),
            origin: r.get("origin"),
            last_activity_at: r.get("last_activity_at"),
            created_at: r.get("created_at"),
        }))
    }

    pub async fn upsert(&self, m: &ThreadMapping) -> Result<(), QaError> {
        sqlx::query(
            "INSERT INTO qa_thread_agent (thread_ts, terminal_id, repo, origin, last_activity_at, created_at)
                  VALUES ($1, $2, $3, $4, $5, $6)
                  ON CONFLICT (thread_ts) DO UPDATE
                    SET terminal_id      = EXCLUDED.terminal_id,
                        repo             = EXCLUDED.repo,
                        last_activity_at = EXCLUDED.last_activity_at",
        )
        .bind(&m.thread_ts)
        .bind(&m.terminal_id)
        .bind(&m.repo)
        .bind(&m.origin)
        .bind(m.last_activity_at)
        .bind(m.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch(&self, thread_ts: &str) -> Result<(), QaError> {
        let now = self.clock.now();
        sqlx::query("UPDATE qa_thread_agent SET last_activity_at = $2 WHERE thread_ts = $1")
            .bind(thread_ts)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_idle(
        &self,
        idle_before: DateTime<Utc>,
    ) -> Result<Vec<ThreadMapping>, QaError> {
        let rows = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, origin, last_activity_at, created_at
               FROM qa_thread_agent
              WHERE last_activity_at < $1",
        )
        .bind(idle_before)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ThreadMapping {
                thread_ts: r.get("thread_ts"),
                terminal_id: r.get("terminal_id"),
                repo: r.get("repo"),
                origin: r.get("origin"),
                last_activity_at: r.get("last_activity_at"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub async fn delete(&self, thread_ts: &str) -> Result<(), QaError> {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(thread_ts)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_all(&self) -> Result<Vec<ThreadMapping>, QaError> {
        let rows = sqlx::query(
            "SELECT thread_ts, terminal_id, repo, origin, last_activity_at, created_at FROM qa_thread_agent",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ThreadMapping {
                thread_ts: r.get("thread_ts"),
                terminal_id: r.get("terminal_id"),
                repo: r.get("repo"),
                origin: r.get("origin"),
                last_activity_at: r.get("last_activity_at"),
                created_at: r.get("created_at"),
            })
            .collect())
    }
}
