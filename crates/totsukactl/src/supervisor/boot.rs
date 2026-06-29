use crate::child::{ChildSpawner, ChildSpec};
use crate::error::TotsukactlError;
use crate::health::HealthProbe;
use crate::paths::Paths;
use crate::pidfile;
use crate::registry::Registry;
use crate::state::ChildState;
use futures_util::future::join_all;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use totsuka_core::Clock;

pub struct BootCtx {
    pub spawner: Arc<dyn ChildSpawner>,
    pub probe: Arc<dyn HealthProbe>,
    pub registry: Arc<Registry>,
    pub clock: Arc<dyn Clock>,
    pub paths: Paths,
    pub ready_timeout: Duration,
}

pub async fn await_ready(
    probe: Arc<dyn HealthProbe>,
    name: &str,
    timeout: Duration,
) -> Result<(), TotsukactlError> {
    let fut = async {
        loop {
            if probe.readyz(name).await.unwrap_or(false) {
                return Ok::<_, TotsukactlError>(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::time::timeout(timeout, fut).await.map_err(|_| {
        TotsukactlError::Timeout(format!("{name} readyz did not arrive in {timeout:?}"))
    })?
}

pub async fn boot<F1, F2>(
    ctx: &BootCtx,
    specs: &[ChildSpec],
    wait_for_pgmq_ready: F1,
    run_phase_0: F2,
) -> Result<(), TotsukactlError>
where
    F1: Future<Output = Result<(), TotsukactlError>>,
    F2: Future<Output = Result<(), TotsukactlError>>,
{
    wait_for_pgmq_ready.await?;
    ctx.registry.set_state("pgmq", ChildState::Healthy).await;

    run_phase_0.await?;

    let mut spawned: Vec<(String, i32)> = Vec::new();

    let phases: Vec<&[&str]> = vec![
        &["agent-adapter"],
        &["orchestrator"],
        &["github-watcher", "qa-service"],
    ];

    for phase in phases {
        let mut pids_this_phase = Vec::new();
        // spawn sequentially within a phase (spawn is fast); await readyz concurrently below.
        for name in phase {
            let spec = specs
                .iter()
                .find(|s| s.name == *name)
                .ok_or_else(|| TotsukactlError::Internal(format!("missing spec for {name}")))?;
            ctx.registry.set_state(name, ChildState::Starting).await;
            let pid = match ctx.spawner.spawn(spec).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(child=name, error=%e, "spawn failed; aborting boot");
                    rollback(&spawned).await;
                    return Err(e);
                }
            };
            let now = ctx.clock.now();
            ctx.registry.set_pid(name, Some(pid), Some(now)).await;
            pidfile::write_pid(&ctx.paths.child_pid(name), pid)?;
            spawned.push(((*name).to_string(), pid));
            pids_this_phase.push((*name, pid));
        }
        // await readyz in parallel
        let waits: Vec<_> = pids_this_phase
            .iter()
            .map(|(n, _)| {
                let p = ctx.probe.clone();
                let n = (*n).to_string();
                let to = ctx.ready_timeout;
                async move { (n.clone(), await_ready(p, &n, to).await) }
            })
            .collect();
        let results = join_all(waits).await;
        for (name, res) in results {
            match res {
                Ok(()) => ctx.registry.set_state(&name, ChildState::Ready).await,
                Err(e) => {
                    tracing::error!(child=%name, error=%e, "readyz timed out; aborting boot");
                    rollback(&spawned).await;
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

async fn rollback(spawned: &[(String, i32)]) {
    for (name, pid) in spawned.iter().rev() {
        let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        tracing::warn!(child=%name, pid, "boot rollback SIGTERM");
    }
}
