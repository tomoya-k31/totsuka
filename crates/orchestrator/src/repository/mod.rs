use async_trait::async_trait;
use chrono::{DateTime, Utc};
use totsuka_core::TaskId;

use crate::error::OrchestratorError;

pub mod postgres;
pub use postgres::PgRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub task_id_short: String,
    pub repo: String,
    pub pr_node_id: Option<String>,
    pub current_column: String,
    pub current_phase: Option<String>,
    pub impl_verify_attempt: i32,
    pub suppress_writeback_until_human_move: bool,
    pub spawned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait Repository: Send + Sync {
    async fn get(&self, id: &TaskId) -> Result<Option<Task>, OrchestratorError>;
    async fn upsert(&self, t: &Task) -> Result<(), OrchestratorError>;
    async fn bump_attempt(&self, id: &TaskId) -> Result<i32, OrchestratorError>;
    async fn set_pr(&self, id: &TaskId, pr: &str) -> Result<(), OrchestratorError>;
    async fn set_suppress(&self, id: &TaskId, v: bool) -> Result<(), OrchestratorError>;
    async fn set_spawned_at(
        &self,
        id: &TaskId,
        when: DateTime<Utc>,
    ) -> Result<(), OrchestratorError>;
    async fn find_by_short(&self, short: &str) -> Result<Option<Task>, OrchestratorError>;
    async fn list_awaiting_release_in_repo(
        &self,
        repo: &str,
    ) -> Result<Vec<Task>, OrchestratorError>;
    async fn list_overdue(
        &self,
        deadline: DateTime<Utc>,
        phase: &str,
    ) -> Result<Vec<Task>, OrchestratorError>;
}
