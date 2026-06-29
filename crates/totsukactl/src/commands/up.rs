use crate::error::TotsukactlError;
use crate::paths::Paths;
use crate::pidfile;
use crate::supervisor::run_supervisor;
use totsuka_config::Config;

pub async fn run(
    mut cfg: Config,
    paths: Paths,
    recreate: bool,
    bootstrap: bool,
) -> Result<(), TotsukactlError> {
    if bootstrap {
        let config_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| crate::paths::resolve_tilde("~/.config"))
            .join("totsuka");
        let cfg_path = config_dir.join("config.toml");
        let sec_path = config_dir.join("secrets.toml");
        if !cfg_path.exists() || !sec_path.exists() {
            tracing::info!("--bootstrap: config files missing, running init");
            crate::commands::init::run(&paths).await?;
            cfg = totsuka_config::Config::load(&cfg_path).map_err(|e| {
                TotsukactlError::Config(format!("re-loading after bootstrap: {e:?}"))
            })?;
            // Re-derive paths from the freshly-loaded config
            let paths = crate::paths::Paths::from_config(&cfg);
            paths.ensure()?;
        }
    }

    match pidfile::check(&paths.supervisor_pid())? {
        pidfile::PidState::Alive(pid) => {
            return Err(TotsukactlError::AlreadyRunning(format!(
                "supervisor pid {pid}"
            )));
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
