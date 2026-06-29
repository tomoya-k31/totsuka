use crate::state::ChildState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const ORDER: &[&str] = &[
    "pgmq",
    "agent-adapter",
    "orchestrator",
    "github-watcher",
    "qa-service",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub name: String,
    pub pid: Option<i32>,
    pub state: ChildState,
    pub started_at: Option<DateTime<Utc>>,
    pub last_healthz_at: Option<DateTime<Utc>>,
    pub last_readyz_at: Option<DateTime<Utc>>,
    pub last_restart_attempt_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub restart_count: u32,
}

impl ProcessEntry {
    pub fn fresh(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pid: None,
            state: ChildState::Stopped,
            started_at: None,
            last_healthz_at: None,
            last_readyz_at: None,
            last_restart_attempt_at: None,
            consecutive_failures: 0,
            restart_count: 0,
        }
    }
}

#[derive(Default, Clone)]
pub struct Registry {
    inner: Arc<RwLock<BTreeMap<String, ProcessEntry>>>,
}

impl Registry {
    pub fn new() -> Self {
        let mut map = BTreeMap::new();
        for name in ORDER {
            map.insert((*name).to_string(), ProcessEntry::fresh(*name));
        }
        Self {
            inner: Arc::new(RwLock::new(map)),
        }
    }

    pub async fn upsert(&self, e: ProcessEntry) {
        self.inner.write().await.insert(e.name.clone(), e);
    }

    pub async fn get(&self, name: &str) -> Option<ProcessEntry> {
        self.inner.read().await.get(name).cloned()
    }

    pub async fn list(&self) -> Vec<ProcessEntry> {
        let map = self.inner.read().await;
        ORDER.iter().filter_map(|n| map.get(*n).cloned()).collect()
    }

    pub async fn set_state(&self, name: &str, state: ChildState) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.state = state;
        }
    }

    pub async fn set_pid(&self, name: &str, pid: Option<i32>, started_at: Option<DateTime<Utc>>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.pid = pid;
            e.started_at = started_at;
        }
    }

    pub async fn bump_failure(&self, name: &str) -> u32 {
        let mut g = self.inner.write().await;
        let e = g
            .entry(name.to_string())
            .or_insert_with(|| ProcessEntry::fresh(name));
        e.consecutive_failures = e.consecutive_failures.saturating_add(1);
        e.consecutive_failures
    }

    pub async fn reset_failure(&self, name: &str) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.consecutive_failures = 0;
        }
    }

    pub async fn bump_restart(&self, name: &str) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.restart_count = e.restart_count.saturating_add(1);
        }
    }

    pub async fn touch_healthz(&self, name: &str, at: DateTime<Utc>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.last_healthz_at = Some(at);
        }
    }

    pub async fn touch_readyz(&self, name: &str, at: DateTime<Utc>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.last_readyz_at = Some(at);
        }
    }

    pub async fn touch_restart_attempt(&self, name: &str, at: DateTime<Utc>) {
        if let Some(e) = self.inner.write().await.get_mut(name) {
            e.last_restart_attempt_at = Some(at);
        }
    }
}
