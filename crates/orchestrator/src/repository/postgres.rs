use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use totsuka_core::{Clock, TaskId};

use super::{Repository, Task};
use crate::error::OrchestratorError;

pub struct PgRepository {
    pool: PgPool,
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
}

impl PgRepository {
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }
}

#[async_trait]
impl Repository for PgRepository {
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, OrchestratorError> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
                String,
                Option<String>,
                i32,
                bool,
                Option<DateTime<Utc>>,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "SELECT id, task_id_short, repo, issue_number, pr_node_id, current_column,
                    current_phase, impl_verify_attempt, suppress_writeback_until_human_move,
                    spawned_at, created_at, updated_at FROM tasks WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Task {
            id: TaskId::new(r.0),
            task_id_short: r.1,
            repo: r.2,
            issue_number: r.3,
            pr_node_id: r.4,
            current_column: r.5,
            current_phase: r.6,
            impl_verify_attempt: r.7,
            suppress_writeback_until_human_move: r.8,
            spawned_at: r.9,
            created_at: r.10,
            updated_at: r.11,
        }))
    }

    async fn upsert(&self, t: &Task) -> Result<(), OrchestratorError> {
        sqlx::query(
            "INSERT INTO tasks (id, task_id_short, repo, issue_number, pr_node_id,
                                current_column, current_phase, impl_verify_attempt,
                                suppress_writeback_until_human_move, spawned_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (id) DO UPDATE SET
                 task_id_short = excluded.task_id_short,
                 repo = excluded.repo,
                 issue_number = COALESCE(excluded.issue_number, tasks.issue_number),
                 pr_node_id = excluded.pr_node_id,
                 current_column = excluded.current_column,
                 current_phase = excluded.current_phase,
                 impl_verify_attempt = excluded.impl_verify_attempt,
                 suppress_writeback_until_human_move = excluded.suppress_writeback_until_human_move,
                 spawned_at = excluded.spawned_at,
                 updated_at = now()",
        )
        .bind(t.id.as_str())
        .bind(&t.task_id_short)
        .bind(&t.repo)
        .bind(t.issue_number)
        .bind(&t.pr_node_id)
        .bind(&t.current_column)
        .bind(&t.current_phase)
        .bind(t.impl_verify_attempt)
        .bind(t.suppress_writeback_until_human_move)
        .bind(t.spawned_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn bump_attempt(&self, id: &TaskId) -> Result<i32, OrchestratorError> {
        let row: (i32,) = sqlx::query_as(
            "UPDATE tasks SET impl_verify_attempt = impl_verify_attempt + 1, updated_at = now()
             WHERE id = $1 RETURNING impl_verify_attempt",
        )
        .bind(id.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    async fn set_pr(&self, id: &TaskId, pr: &str) -> Result<(), OrchestratorError> {
        sqlx::query("UPDATE tasks SET pr_node_id = $2, updated_at = now() WHERE id = $1")
            .bind(id.as_str())
            .bind(pr)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_suppress(&self, id: &TaskId, v: bool) -> Result<(), OrchestratorError> {
        sqlx::query(
            "UPDATE tasks SET suppress_writeback_until_human_move = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id.as_str())
        .bind(v)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_spawned_at(
        &self,
        id: &TaskId,
        when: DateTime<Utc>,
    ) -> Result<(), OrchestratorError> {
        sqlx::query("UPDATE tasks SET spawned_at = $2, updated_at = now() WHERE id = $1")
            .bind(id.as_str())
            .bind(when)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn find_by_short(&self, short: &str) -> Result<Option<Task>, OrchestratorError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT id FROM tasks WHERE task_id_short = $1")
                .bind(short)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((id,)) => self.get(&TaskId::new(id)).await,
            None => Ok(None),
        }
    }

    async fn list_awaiting_release_in_repo(
        &self,
        repo: &str,
    ) -> Result<Vec<Task>, OrchestratorError> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
                String,
                Option<String>,
                i32,
                bool,
                Option<DateTime<Utc>>,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "SELECT id, task_id_short, repo, issue_number, pr_node_id, current_column,
                    current_phase, impl_verify_attempt, suppress_writeback_until_human_move,
                    spawned_at, created_at, updated_at
             FROM tasks WHERE repo = $1 AND current_column = 'awaiting_release'",
        )
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Task {
                id: TaskId::new(r.0),
                task_id_short: r.1,
                repo: r.2,
                issue_number: r.3,
                pr_node_id: r.4,
                current_column: r.5,
                current_phase: r.6,
                impl_verify_attempt: r.7,
                suppress_writeback_until_human_move: r.8,
                spawned_at: r.9,
                created_at: r.10,
                updated_at: r.11,
            })
            .collect())
    }

    async fn list_overdue(
        &self,
        deadline: DateTime<Utc>,
        phase: &str,
    ) -> Result<Vec<Task>, OrchestratorError> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
                String,
                Option<String>,
                i32,
                bool,
                Option<DateTime<Utc>>,
                DateTime<Utc>,
                DateTime<Utc>,
            ),
        >(
            "SELECT id, task_id_short, repo, issue_number, pr_node_id, current_column,
                    current_phase, impl_verify_attempt, suppress_writeback_until_human_move,
                    spawned_at, created_at, updated_at FROM tasks
             WHERE current_phase = $2 AND spawned_at IS NOT NULL AND spawned_at < $1",
        )
        .bind(deadline)
        .bind(phase)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Task {
                id: TaskId::new(r.0),
                task_id_short: r.1,
                repo: r.2,
                issue_number: r.3,
                pr_node_id: r.4,
                current_column: r.5,
                current_phase: r.6,
                impl_verify_attempt: r.7,
                suppress_writeback_until_human_move: r.8,
                spawned_at: r.9,
                created_at: r.10,
                updated_at: r.11,
            })
            .collect())
    }
}
