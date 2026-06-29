use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::supervisor::run_supervisor;
use totsuka_config::Config;

pub async fn run(
    cfg: Config,
    paths: Paths,
    recreate: bool,
    _bootstrap: bool,
) -> Result<(), TotsukactlError> {
    match pidfile::check(&paths.supervisor_pid())? {
        pidfile::PidState::Alive(pid) => {
            return Err(TotsukactlError::AlreadyRunning(format!("supervisor pid {pid}")));
        }
        pidfile::PidState::Stale(pid) => {
            tracing::warn!(stale_pid = pid, "removing stale supervisor.pid");
            pidfile::remove(&paths.supervisor_pid())?;
        }
        pidfile::PidState::Absent => {}
    }

    paths.ensure()?;
    pidfile::write_pid(&paths.supervisor_pid(), std::process::id() as i32)?;

    let result = run_supervisor(cfg, paths.clone(), recreate).await;
    let _ = pidfile::remove(&paths.supervisor_pid());
    result
}
