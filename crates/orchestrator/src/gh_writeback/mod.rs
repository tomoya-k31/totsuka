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
    /// Update the Project item's status column. Returns Ok | VersionMismatch | Failed.
    ///
    /// `expected_version`: an opaque token the backend may use for OCC. The
    /// GitHub GraphQL backend ignores this (the ProjectsV2 mutation has no
    /// native OCC field) and instead detects conflicts by parsing the response
    /// for "stale" / type=="CONFLICT". Other backends (e.g. a future
    /// REST-based one) may participate in OCC by passing the value from a
    /// prior fetch.
    async fn move_column(
        &self,
        task_id: &str,
        to_column: &str,
        expected_version: Option<String>,
    ) -> Result<WritebackResult, OrchestratorError>;
}
