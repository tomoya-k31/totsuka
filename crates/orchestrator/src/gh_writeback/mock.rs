use super::{WritebackClient, WritebackResult};
use crate::error::OrchestratorError;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
pub struct MockWriteback {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    pub moves: Vec<(String, String, Option<String>)>,
    pub next_result: Option<WritebackResult>,
}

impl MockWriteback {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_next(&self, r: WritebackResult) {
        self.state.lock().unwrap().next_result = Some(r);
    }

    pub fn moves(&self) -> Vec<(String, String, Option<String>)> {
        self.state.lock().unwrap().moves.clone()
    }
}

#[async_trait]
impl WritebackClient for MockWriteback {
    async fn move_column(
        &self,
        task_id: &str,
        to_column: &str,
        version: Option<String>,
    ) -> Result<WritebackResult, OrchestratorError> {
        let mut g = self.state.lock().unwrap();
        g.moves.push((task_id.into(), to_column.into(), version));
        Ok(g.next_result.take().unwrap_or(WritebackResult::Ok))
    }
}
