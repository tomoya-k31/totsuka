use crate::compose::ComposeExec;
use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::state::ChildState;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::sync::Arc;
use std::time::Duration;

pub struct ShutdownCfg {
    pub grace: Duration,
    pub second_term: Duration,
    pub force_grace: Duration,
    pub also_postgres: bool,
    pub force: bool,
}

pub async fn shutdown_stack(
    cfg: ShutdownCfg,
    registry: Arc<Registry>,
    compose: Arc<dyn ComposeExec>,
    paths: Paths,
) -> Result<(), TotsukactlError> {
    if cfg.force {
        let all = [
            "github-watcher",
            "qa-service",
            "orchestrator",
            "agent-adapter",
        ];
        sigterm_parallel(&registry, &all).await;
        wait_or_kill(&registry, &all, cfg.force_grace).await;
    } else {
        // stage 1: ingestion
        sigterm_parallel(&registry, &["github-watcher", "qa-service"]).await;
        wait_or_kill_escalate(
            &registry,
            &["github-watcher", "qa-service"],
            cfg.grace,
            cfg.second_term,
        )
        .await;
        // stage 2: control
        sigterm_parallel(&registry, &["orchestrator"]).await;
        wait_or_kill_escalate(&registry, &["orchestrator"], cfg.grace, cfg.second_term).await;
        // stage 3: execution
        sigterm_parallel(&registry, &["agent-adapter"]).await;
        wait_or_kill_escalate(&registry, &["agent-adapter"], cfg.grace, cfg.second_term).await;
    }

    for n in [
        "github-watcher",
        "qa-service",
        "orchestrator",
        "agent-adapter",
    ] {
        registry.set_state(n, ChildState::Stopped).await;
        pidfile::remove(&paths.child_pid(n))?;
    }

    pidfile::remove(&paths.supervisor_pid())?;

    if cfg.also_postgres {
        compose.stop("pgmq").await?;
        registry.set_state("pgmq", ChildState::Stopped).await;
    }
    Ok(())
}

async fn sigterm_parallel(registry: &Registry, names: &[&str]) {
    for n in names {
        if let Some(e) = registry.get(n).await {
            if let Some(pid) = e.pid {
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
                tracing::info!(child = *n, pid, "SIGTERM");
            }
        }
    }
}

async fn wait_or_kill_escalate(
    registry: &Registry,
    names: &[&str],
    grace: Duration,
    second: Duration,
) {
    tokio::time::sleep(grace).await;
    let still: Vec<_> = collect_alive(registry, names).await;
    for (n, pid) in &still {
        let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        tracing::warn!(child = %n, pid, "SIGTERM (2nd)");
    }
    tokio::time::sleep(second).await;
    for (n, pid) in collect_alive(registry, names).await {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        tracing::error!(child = %n, pid, "SIGKILL");
    }
}

async fn wait_or_kill(registry: &Registry, names: &[&str], grace: Duration) {
    tokio::time::sleep(grace).await;
    for (n, pid) in collect_alive(registry, names).await {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        tracing::error!(child = %n, pid, "SIGKILL (force)");
    }
}

async fn collect_alive(registry: &Registry, names: &[&str]) -> Vec<(String, i32)> {
    let mut out = Vec::new();
    for n in names {
        if let Some(e) = registry.get(n).await {
            if let Some(pid) = e.pid {
                if pidfile::process_alive(pid) {
                    out.push(((*n).to_string(), pid));
                }
            }
        }
    }
    out
}
