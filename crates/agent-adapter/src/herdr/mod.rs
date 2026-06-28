//! Abstraction over the herdr daemon socket. Spec §8.1: agent-adapter is the
//! sole adapter from totsuka domain types into herdr's native Unix-socket API
//! (NDJSON). The concrete wire impl lives in [`wire`]; tests use [`mock`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod mock;
pub mod wire;

/// herdr-assigned identifier for a single Claude pane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Snapshot returned by `pane.read`. `revision` is herdr's monotone update
/// counter; callers should poll until it advances past their previous value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub revision: u64,
    pub text: String,
}

impl PaneSnapshot {
    pub fn is_newer_than(&self, prev_rev: u64) -> bool {
        self.revision > prev_rev
    }
}

/// Parameters for `agent.start`. `argv` is the Claude Code invocation; `env`
/// carries secrets (spec §11.13: never in argv).
#[derive(Clone, Serialize)]
pub struct SpawnRequest {
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: HashMap<String, String>,
    pub label: String, // herdr pane label (e.g. "totsuka:abc123:implv")
}

/// Hand-written Debug that elides env values to prevent accidental secret
/// leakage via `tracing::debug!(?req)` (spec §11.7).
impl std::fmt::Debug for SpawnRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnRequest")
            .field("cwd", &self.cwd)
            .field("argv", &self.argv)
            .field(
                "env",
                &format_args!("<{} entries: redacted>", self.env.len()),
            )
            .field("label", &self.label)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnResult {
    pub agent_id: AgentId,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub agent_id: AgentId,
    pub label: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    #[error("herdr io: {0}")]
    Io(#[from] std::io::Error),
    #[error("herdr returned error: {code} {message}")]
    Remote { code: String, message: String },
    #[error("herdr returned malformed response: {0}")]
    Decode(String),
}

#[async_trait]
pub trait HerdrClient: Send + Sync {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError>;
    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError>;
    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError>;
    async fn close(&self, id: &AgentId) -> Result<(), HerdrError>;
    async fn list(&self) -> Result<Vec<ListItem>, HerdrError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_serde_transparent() {
        let id = AgentId::new("ag_123".to_string());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ag_123\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn pane_snapshot_revision_monotone_helper() {
        let s = PaneSnapshot {
            revision: 4,
            text: "hello".into(),
        };
        assert!(s.is_newer_than(3));
        assert!(!s.is_newer_than(4));
    }

    #[test]
    fn spawn_request_debug_does_not_leak_env_values() {
        let mut env = HashMap::new();
        env.insert("CLAUDE_TOKEN".to_string(), "tk_secret_123".to_string());
        let req = SpawnRequest {
            cwd: "/w".into(),
            argv: vec!["claude".into()],
            env,
            label: "t".into(),
        };
        let dbg = format!("{:?}", req);
        assert!(
            !dbg.contains("tk_secret_123"),
            "Debug leaked env value: {dbg}"
        );
        assert!(!dbg.contains("CLAUDE_TOKEN"), "Debug leaked env key: {dbg}");
        assert!(
            dbg.contains("redacted"),
            "Debug should mark env as redacted: {dbg}"
        );
    }
}
