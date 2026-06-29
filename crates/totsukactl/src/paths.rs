//! XDG runtime layout per spec §10.2.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub pid_dir: PathBuf,
    pub sock_dir: PathBuf,
}

pub fn resolve_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

impl Paths {
    pub fn from_config(cfg: &totsuka_config::Config) -> Self {
        let state_dir = resolve_tilde(&cfg.totsuka.state_dir);
        let data_dir = resolve_tilde(&cfg.totsuka.data_dir);
        Self {
            log_dir: state_dir.join("logs"),
            pid_dir: state_dir.join("pids"),
            sock_dir: state_dir.join("sock"),
            state_dir,
            data_dir,
        }
    }

    pub fn supervisor_pid(&self) -> PathBuf {
        self.state_dir.join("supervisor.pid")
    }
    pub fn supervisor_sock(&self) -> PathBuf {
        self.sock_dir.join("supervisor.sock")
    }
    pub fn supervisor_log(&self) -> PathBuf {
        self.log_dir.join("supervisor.log")
    }
    pub fn child_pid(&self, bin: &str) -> PathBuf {
        self.pid_dir.join(format!("{bin}.pid"))
    }
    pub fn child_log(&self, bin: &str) -> PathBuf {
        self.log_dir.join(format!("{bin}.log"))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for p in [&self.state_dir, &self.data_dir, &self.log_dir, &self.pid_dir, &self.sock_dir] {
            std::fs::create_dir_all(p)?;
        }
        chmod_0700(&self.sock_dir)?;
        Ok(())
    }
}

#[cfg(unix)]
fn chmod_0700(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(p, perms)
}

#[cfg(not(unix))]
fn chmod_0700(_p: &Path) -> std::io::Result<()> {
    Ok(())
}
