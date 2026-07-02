//! In-memory `HerdrClient` for tests. Maintains a `HashMap<AgentId, Pane>`
//! under a Mutex; revision advances on `send` and persists across `read`.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{AgentId, HerdrClient, HerdrError, ListItem, PaneSnapshot, SpawnRequest, SpawnResult};

#[derive(Debug, Default)]
struct Pane {
    label: String,
    text: String,
    revision: u64,
}

#[derive(Debug, Default, Clone)]
pub struct MockHerdr {
    inner: Arc<Mutex<HashMap<AgentId, Pane>>>,
    last_spawn: Arc<Mutex<Option<SpawnRequest>>>,
}

impl MockHerdr {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: count of in-flight panes.
    pub fn count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Test helper: the most recent SpawnRequest passed to `start`.
    pub fn last_spawn(&self) -> Option<SpawnRequest> {
        self.last_spawn.lock().unwrap().clone()
    }
}

#[async_trait]
impl HerdrClient for MockHerdr {
    async fn start(&self, req: SpawnRequest) -> Result<SpawnResult, HerdrError> {
        *self.last_spawn.lock().unwrap() = Some(req.clone());
        let id = AgentId::new(format!("ag_{}", Uuid::new_v4().simple()));
        let pane = Pane {
            label: req.label,
            text: String::new(),
            revision: 0,
        };
        self.inner.lock().unwrap().insert(id.clone(), pane);
        Ok(SpawnResult {
            agent_id: id.clone(),
            terminal_id: format!("term_{}", id.as_str()),
        })
    }

    async fn send(&self, id: &AgentId, text: &str) -> Result<(), HerdrError> {
        let mut g = self.inner.lock().unwrap();
        let pane = g.get_mut(id).ok_or_else(|| HerdrError::Remote {
            code: "not_found".into(),
            message: format!("unknown agent {}", id.as_str()),
        })?;
        pane.text.push_str(text);
        pane.revision += 1;
        Ok(())
    }

    async fn read(&self, id: &AgentId) -> Result<PaneSnapshot, HerdrError> {
        let g = self.inner.lock().unwrap();
        let pane = g.get(id).ok_or_else(|| HerdrError::Remote {
            code: "not_found".into(),
            message: format!("unknown agent {}", id.as_str()),
        })?;
        Ok(PaneSnapshot {
            revision: pane.revision,
            text: pane.text.clone(),
        })
    }

    async fn close(&self, id: &AgentId) -> Result<(), HerdrError> {
        let mut g = self.inner.lock().unwrap();
        if g.remove(id).is_none() {
            return Err(HerdrError::Remote {
                code: "not_found".into(),
                message: format!("unknown agent {}", id.as_str()),
            });
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ListItem>, HerdrError> {
        let g = self.inner.lock().unwrap();
        Ok(g.iter()
            .map(|(id, p)| ListItem {
                agent_id: id.clone(),
                label: p.label.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{HerdrClient, SpawnRequest};
    use std::collections::HashMap;

    #[tokio::test]
    async fn start_then_list_then_close() {
        let h = MockHerdr::new();
        let id = h
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec!["claude".into()],
                env: HashMap::new(),
                label: "totsuka:t1:design".into(),
            })
            .await
            .unwrap()
            .agent_id;
        let list = h.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_id, id);
        h.close(&id).await.unwrap();
        assert!(h.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_returns_appended_text() {
        let h = MockHerdr::new();
        let id = h
            .start(SpawnRequest {
                cwd: "/w".into(),
                argv: vec![],
                env: HashMap::new(),
                label: "x".into(),
            })
            .await
            .unwrap()
            .agent_id;
        h.send(&id, "hello").await.unwrap();
        h.send(&id, "world").await.unwrap();
        let snap = h.read(&id).await.unwrap();
        assert_eq!(snap.text, "helloworld");
        assert_eq!(snap.revision, 2);
    }

    #[tokio::test]
    async fn close_unknown_errors() {
        let h = MockHerdr::new();
        let err = h.close(&AgentId::new("nope".into())).await.unwrap_err();
        assert!(matches!(err, HerdrError::Remote { .. }));
    }
}
