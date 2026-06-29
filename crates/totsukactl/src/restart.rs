use crate::state::{ChildState, RestartPolicy};
use std::time::Duration;
use totsuka_config::schema::HeartbeatSection;

#[derive(Debug, Clone)]
pub struct RestartCfg {
    pub policy: RestartPolicy,
    pub backoff_secs: Vec<u64>,
    pub max_attempts: u32,
}

impl RestartCfg {
    pub fn from_section(s: &HeartbeatSection) -> Result<Self, crate::error::TotsukactlError> {
        Ok(Self {
            policy: RestartPolicy::parse(&s.restart_policy)?,
            backoff_secs: s.restart_backoff_secs.clone(),
            max_attempts: s.restart_max_attempts,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RestartDecision {
    Skip,
    Wait(Duration),
    GiveUp,
}

pub fn decide(state: ChildState, restart_count: u32, cfg: &RestartCfg) -> RestartDecision {
    let eligible = match cfg.policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnDeadOnly => matches!(state, ChildState::Dead),
        RestartPolicy::OnUnhealthy => matches!(state, ChildState::Dead | ChildState::Unhealthy),
    };
    if !eligible {
        return RestartDecision::Skip;
    }
    if restart_count >= cfg.max_attempts {
        return RestartDecision::GiveUp;
    }
    RestartDecision::Wait(backoff_for(restart_count, cfg))
}

pub fn backoff_for(attempt: u32, cfg: &RestartCfg) -> Duration {
    if cfg.backoff_secs.is_empty() {
        return Duration::from_secs(5);
    }
    let idx = (attempt as usize).min(cfg.backoff_secs.len() - 1);
    Duration::from_secs(cfg.backoff_secs[idx])
}
