use crate::error::OrchestratorError;
use async_trait::async_trait;

pub mod http;
pub mod mock;
pub use mock::MockWriteback;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritebackResult {
    Ok,
    VersionMismatch,
    Failed(String),
}

#[async_trait]
pub trait WritebackClient: Send + Sync {
    async fn move_column(
        &self,
        task_id: &str,
        to_column: &str,
        expected_version: Option<String>,
    ) -> Result<WritebackResult, OrchestratorError>;
}
