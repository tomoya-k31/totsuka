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
    /// QA agents only answer questions — no branch, detached worktree.
    pub detached: bool,
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
            .field("detached", &self.detached)
            .field(
                "env",
                &format_args!("<{} entries: redacted>", self.env.len()),
            )
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
    pub detached: bool,
}

/// True when an adapter error means the target agent no longer exists:
/// an explicit `404` from the adapter, or herdr reporting `agent_not_found`
/// (which the send/read handlers surface as `503 herdr_unavailable`).
/// Scoped to `QaError::Adapter` so unrelated variants can never match —
/// `HyperlocalAdapter` formats these messages as `"{status} {path}: {body}"`.
pub fn is_agent_gone(e: &crate::error::QaError) -> bool {
    match e {
        crate::error::QaError::Adapter(msg) => msg.starts_with("404 ") || msg.contains("not_found"),
        _ => false,
    }
}

#[cfg(test)]
mod agent_gone_tests {
    use super::is_agent_gone;
    use crate::error::QaError;

    #[test]
    fn explicit_404_is_gone() {
        assert!(is_agent_gone(&QaError::Adapter(
            "404 /v1/agents/t1: not found".into()
        )));
    }

    #[test]
    fn herdr_agent_not_found_via_503_is_gone() {
        assert!(is_agent_gone(&QaError::Adapter(
            "503 /v1/agents/t1/messages: {\"detail\":\"agent_not_found t1\"}".into()
        )));
    }

    #[test]
    fn transient_adapter_error_is_not_gone() {
        assert!(!is_agent_gone(&QaError::Adapter(
            "503 /v1/agents/t1/messages: herdr unavailable: connect".into()
        )));
    }

    #[test]
    fn non_adapter_variants_never_match() {
        assert!(!is_agent_gone(&QaError::Slack("channel not_found".into())));
    }
}
