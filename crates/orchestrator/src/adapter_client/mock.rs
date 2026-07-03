use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{AdapterClient, ReadRes, SpawnReq, SpawnRes};
use crate::error::OrchestratorError;

#[derive(Default, Clone)]
pub struct MockAdapter {
    state: Arc<Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    pub agents: HashMap<String, MockPane>,
    pub spawn_log: Vec<SpawnReq>,
    pub send_log: Vec<(String, String)>,
}

#[derive(Default, Clone)]
struct MockPane {
    pub text: String,
    pub revision: u64,
    #[allow(dead_code)]
    pub repo: String,
    #[allow(dead_code)]
    pub branch: String,
}

impl MockAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn_count(&self) -> usize {
        self.state.lock().unwrap().spawn_log.len()
    }

    pub fn last_spawn(&self) -> Option<SpawnReq> {
        self.state.lock().unwrap().spawn_log.last().cloned()
    }

    pub fn send_count(&self) -> usize {
        self.state.lock().unwrap().send_log.len()
    }

    /// Test helper: most recent (agent_id, text) passed to `send`.
    pub fn last_send(&self) -> Option<(String, String)> {
        self.state.lock().unwrap().send_log.last().cloned()
    }

    pub fn set_pane_text(&self, agent_id: &str, text: &str) {
        let mut g = self.state.lock().unwrap();
        if let Some(p) = g.agents.get_mut(agent_id) {
            p.text = text.into();
            p.revision += 1;
        }
    }
}

#[async_trait]
impl AdapterClient for MockAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, OrchestratorError> {
        let agent_id = format!("ag_{}", Uuid::new_v4().simple());
        let terminal_id = format!("term_{}", agent_id);
        let worktree_path = format!("/tmp/{}", req.branch.replace('/', "__"));
        let mut g = self.state.lock().unwrap();
        g.agents.insert(
            agent_id.clone(),
            MockPane {
                text: String::new(),
                revision: 0,
                repo: req.repo.clone(),
                branch: req.branch.clone(),
            },
        );
        g.spawn_log.push(req);
        Ok(SpawnRes {
            agent_id,
            terminal_id,
            worktree_path,
        })
    }

    async fn send(&self, agent_id: &str, text: &str) -> Result<(), OrchestratorError> {
        let mut g = self.state.lock().unwrap();
        if let Some(p) = g.agents.get_mut(agent_id) {
            p.text.push_str(text);
            p.revision += 1;
        }
        g.send_log.push((agent_id.into(), text.into()));
        Ok(())
    }

    async fn read(
        &self,
        agent_id: &str,
        since_revision: u64,
    ) -> Result<ReadRes, OrchestratorError> {
        let g = self.state.lock().unwrap();
        let p = g
            .agents
            .get(agent_id)
            .ok_or_else(|| OrchestratorError::Adapter(format!("unknown agent {agent_id}")))?;
        Ok(ReadRes {
            revision: p.revision,
            text: p.text.clone(),
            is_newer: p.revision > since_revision,
        })
    }

    async fn stop(
        &self,
        agent_id: &str,
        _repo: &str,
        _branch: &str,
    ) -> Result<(), OrchestratorError> {
        self.state.lock().unwrap().agents.remove(agent_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_then_read_then_stop() {
        let a = MockAdapter::new();
        let res = a
            .spawn(SpawnReq {
                task_id: "t".into(),
                phase: "design".into(),
                attempt: 0,
                repo: "x/y".into(),
                branch: "totsuka/aaaaaaaaaaaa/design".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                detached: false,
            })
            .await
            .unwrap();
        assert_eq!(a.spawn_count(), 1);
        a.send(&res.agent_id, "hi").await.unwrap();
        let r = a.read(&res.agent_id, 0).await.unwrap();
        assert_eq!(r.text, "hi");
        assert!(r.is_newer);
        a.stop(&res.agent_id, "x/y", "totsuka/aaaaaaaaaaaa/design")
            .await
            .unwrap();
    }
}
