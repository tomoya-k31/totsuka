use crate::error::TotsukactlError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildState {
    Starting,
    Ready,
    Healthy,
    Degraded,
    Unhealthy,
    Dead,
    Restarting,
    GivingUp,
    Draining,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackState {
    Stopped,
    Starting,
    Running,
    Degraded,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    OnDeadOnly,
    OnUnhealthy,
    Never,
}

impl RestartPolicy {
    pub fn parse(s: &str) -> Result<Self, TotsukactlError> {
        match s {
            "on-dead-only" => Ok(Self::OnDeadOnly),
            "on-unhealthy" => Ok(Self::OnUnhealthy),
            "never" => Ok(Self::Never),
            other => Err(TotsukactlError::Config(format!(
                "unknown restart_policy {other:?} (expected on-dead-only|on-unhealthy|never)"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthOutcome {
    Ok,
    Degraded,
    Unhealthy,
    Dead,
}

/// Pure transition function. spec §9.1:
///   Starting → Ready → Healthy
///                    ↘ Degraded   (readyz NG ≥ degraded_threshold)
///                    ↘ Unhealthy  (healthz NG ≥ unhealthy_threshold)
///                    ↘ Dead       (SIGCHLD / connect refused)
///   Dead | Unhealthy → (caller decides Restarting via restart_policy)
///   GivingUp / Draining / Stopped: terminal w.r.t. health ticks; stay put.
pub fn next_state(
    current: ChildState,
    outcome: HealthOutcome,
    consecutive_failures: u32,
    degraded_threshold: u32,
    unhealthy_threshold: u32,
) -> ChildState {
    use ChildState::*;
    use HealthOutcome as HO;
    match (current, outcome) {
        (GivingUp | Draining | Stopped | Restarting, _) => current,
        (_, HO::Dead) => Dead,
        (_, HO::Ok) => Healthy,
        (_, HO::Degraded) if consecutive_failures >= unhealthy_threshold => Unhealthy,
        (_, HO::Degraded) if consecutive_failures >= degraded_threshold => Degraded,
        (_, HO::Degraded) => current,
        (_, HO::Unhealthy) if consecutive_failures >= unhealthy_threshold => Unhealthy,
        (_, HO::Unhealthy) if consecutive_failures >= degraded_threshold => Degraded,
        (_, HO::Unhealthy) => current,
    }
}
