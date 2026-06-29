use crate::registry::ProcessEntry;
use crate::state::ChildState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProcessDto {
    pub name: String,
    pub pid: Option<i32>,
    pub state: ChildState,
    pub started_at: Option<DateTime<Utc>>,
    pub last_healthz_at: Option<DateTime<Utc>>,
    pub last_readyz_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub restart_count: u32,
}

impl From<ProcessEntry> for ProcessDto {
    fn from(e: ProcessEntry) -> Self {
        ProcessDto {
            name: e.name,
            pid: e.pid,
            state: e.state,
            started_at: e.started_at,
            last_healthz_at: e.last_healthz_at,
            last_readyz_at: e.last_readyz_at,
            consecutive_failures: e.consecutive_failures,
            restart_count: e.restart_count,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShutdownReq {
    pub postgres: bool,
    pub force: bool,
}
