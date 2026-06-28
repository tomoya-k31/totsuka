use async_trait::async_trait;
use std::sync::Mutex;

use super::{AdapterClient, AgentSummary, ReadRes, SpawnReq, SpawnRes};
use crate::error::QaError;

#[derive(Default)]
struct MockState {
    spawn_response: Option<SpawnRes>,
    read_response: Option<ReadRes>,
    list_response: Vec<AgentSummary>,
    sends: Vec<(String, String)>,
    stops: Vec<(String, String, String)>,
    spawns: Vec<SpawnReq>,
}

pub struct MockAdapter {
    state: Mutex<MockState>,
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAdapter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MockState::default()),
        }
    }

    pub fn set_spawn_response(&self, r: SpawnRes) {
        self.state.lock().unwrap().spawn_response = Some(r);
    }

    pub fn set_read_response(&self, r: ReadRes) {
        self.state.lock().unwrap().read_response = Some(r);
    }

    pub fn set_list_response(&self, r: Vec<AgentSummary>) {
        self.state.lock().unwrap().list_response = r;
    }

    pub fn expected_sends(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().sends.clone()
    }

    pub fn expected_stops(&self) -> Vec<(String, String, String)> {
        self.state.lock().unwrap().stops.clone()
    }

    pub fn expected_spawns(&self) -> Vec<SpawnReq> {
        self.state.lock().unwrap().spawns.clone()
    }
}

#[async_trait]
impl AdapterClient for MockAdapter {
    async fn spawn(&self, req: SpawnReq) -> Result<SpawnRes, QaError> {
        let mut s = self.state.lock().unwrap();
        s.spawns.push(req);
        s.spawn_response
            .clone()
            .ok_or_else(|| QaError::Adapter("MockAdapter has no spawn_response set".into()))
    }

    async fn send(&self, agent_id: &str, text: &str) -> Result<(), QaError> {
        self.state
            .lock()
            .unwrap()
            .sends
            .push((agent_id.into(), text.into()));
        Ok(())
    }

    async fn read(&self, _agent_id: &str, _since: u64) -> Result<ReadRes, QaError> {
        self.state
            .lock()
            .unwrap()
            .read_response
            .clone()
            .ok_or_else(|| QaError::Adapter("MockAdapter has no read_response set".into()))
    }

    async fn stop(&self, agent_id: &str, repo: &str, branch: &str) -> Result<(), QaError> {
        self.state
            .lock()
            .unwrap()
            .stops
            .push((agent_id.into(), repo.into(), branch.into()));
        Ok(())
    }

    async fn list(&self) -> Result<Vec<AgentSummary>, QaError> {
        Ok(self.state.lock().unwrap().list_response.clone())
    }
}
