use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::sock_api::SupervisorClient;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::time::{Duration, Instant};

pub async fn run(paths: &Paths, force: bool, postgres: bool) -> Result<(), TotsukactlError> {
    let pid_state = pidfile::check(&paths.supervisor_pid())?;
    let maybe_pid: Option<i32> = match pid_state {
        pidfile::PidState::Alive(p) => Some(p),
        pidfile::PidState::Stale(_) | pidfile::PidState::Absent => None,
    };

    let client = SupervisorClient::new(paths.supervisor_sock());

    // If pid is absent or stale, the sock is our only remaining evidence of a
    // live supervisor. Try it before giving up.
    if maybe_pid.is_none() {
        match client.shutdown(postgres, force).await {
            Ok(()) => {
                // Shutdown command was delivered. The supervisor will exit on
                // its own; we have no pid to wait on, so we return Ok.
                tracing::info!(
                    "supervisor.pid was absent but supervisor.sock accepted shutdown; \
                     supervisor will exit on its own"
                );
                let _ = pidfile::remove(&paths.supervisor_pid()); // idempotent cleanup
                return Ok(());
            }
            Err(TotsukactlError::SupervisorUnreachable(_)) => {
                return Err(TotsukactlError::NotRunning);
            }
            Err(e) => return Err(e),
        }
    }

    let pid = maybe_pid.expect("guarded above");
    match client.shutdown(postgres, force).await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!(error=%e, "supervisor.sock shutdown unreachable; falling back to SIGTERM");
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !pidfile::process_alive(pid) {
            pidfile::remove(&paths.supervisor_pid())?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if force {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        pidfile::remove(&paths.supervisor_pid())?;
        Ok(())
    } else {
        Err(TotsukactlError::Timeout(format!(
            "supervisor pid {pid} did not exit in 30s; rerun with --force"
        )))
    }
}
