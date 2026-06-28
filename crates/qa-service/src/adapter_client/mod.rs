//! HTTP-over-UDS client to agent-adapter. Mirrors crates/orchestrator/src/
//! adapter_client/* but adds `list()` for restart recovery (spec §8.4
//! 再起動時のリカバリ — reconcile qa_thread_agent vs adapter's agent.list).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use totsuka_core::Secret;

use crate::error::QaError;

pub mod mock;
pub mod uds;
pub use mock::MockAdapter;
pub use uds::HyperlocalAdapter;

#[derive(Clone)]
pub struct SpawnReq {
    pub task_id: String,
    pub phase: String,
    pub attempt: i32,
    pub repo: String,
    pub branch: String,
    pub argv: Vec<String>,
    pub env: HashMap<String, Secret<String>>,
}

impl std::fmt::Debug for SpawnReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnReq")
            .field("task_id", &self.task_id)
            .field("phase", &self.phase)
            .field("attempt", &self.attempt)
            .field("repo", &self.repo)
            .field("branch", &self.branch)
            .field("argv", &self.argv)
            .field("env", &format_args!("<{} entries: redacted>", self.env.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnRes {
    pub agent_id: String,
    pub terminal_id: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadRes {
    pub revision: u64,
    pub text: String,
    #[serde(default)]
    pub is_newer: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub terminal_id: String,
    pub label: String,
}

#[async_trait]
pub trait AdapterClient: Send + Sync + 'static {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, QaError>;
    async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError>;
    async fn read(&self, agent_id: &str, since_revision: u64) -> Result<ReadRes, QaError>;
    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), QaError>;
    async fn list(&self) -> Result<Vec<AgentSummary>, QaError>;
}

#[derive(Serialize)]
pub(crate) struct WireSpawn<'a> {
    pub task_id: &'a str,
    pub phase: &'a str,
    pub attempt: i32,
    pub repo: &'a str,
    pub branch: &'a str,
    pub argv: &'a [String],
    pub env: HashMap<&'a str, &'a str>,
}
