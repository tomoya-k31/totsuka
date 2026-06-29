use crate::child::{ChildSpawner, ChildSpec};
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::restart::RestartCfg;
use crate::state::ChildState;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

#[allow(clippy::too_many_arguments)]
pub async fn handle_restart(
    name: &str,
    registry: Arc<Registry>,
    spawner: Arc<dyn ChildSpawner>,
    specs: &[ChildSpec],
    paths: &Paths,
    clock: Arc<dyn Clock>,
    restart_cfg: &RestartCfg,
    grace: Duration,
) -> Result<(), TotsukactlError> {
    let spec = specs
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
    if let Some(e) = registry.get(name).await {
        if let Some(pid) = e.pid {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
            registry.set_state(name, ChildState::Draining).await;
            tokio::time::sleep(grace).await;
            if pidfile::process_alive(pid) {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
    }
    registry.set_state(name, ChildState::Restarting).await;
    let cur = registry.get(name).await.map(|e| e.restart_count).unwrap_or(0);
    if cur >= restart_cfg.max_attempts {
        registry.set_state(name, ChildState::GivingUp).await;
        return Err(TotsukactlError::Internal(format!(
            "restart count {cur} >= max_attempts {} -> giving_up",
            restart_cfg.max_attempts
        )));
    }
    registry.set_state(name, ChildState::Starting).await;
    let pid = spawner.spawn(spec).await?;
    let now = clock.now();
    registry.set_pid(name, Some(pid), Some(now)).await;
    pidfile::write_pid(&paths.child_pid(name), pid)?;
    registry.bump_restart(name).await;
    registry.set_state(name, ChildState::Ready).await;
    Ok(())
}

pub async fn handle_reload(name: &str, registry: Arc<Registry>) -> Result<(), TotsukactlError> {
    let e = registry
        .get(name)
        .await
        .ok_or_else(|| TotsukactlError::UnknownChild(name.into()))?;
    let pid = e
        .pid
        .ok_or_else(|| TotsukactlError::Internal(format!("{name} has no pid")))?;
    kill(Pid::from_raw(pid), Signal::SIGHUP)
        .map_err(|e| TotsukactlError::Internal(format!("SIGHUP {pid}: {e}")))?;
    Ok(())
}
