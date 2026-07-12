//! Single-instance run lock (F-74).
//!
//! `run` writes its PID to `$XDG_STATE_HOME/totsuka/run.lock`. If the lock
//! already exists, the holder's liveness is checked via a [`ProcessProbe`]: a
//! dead PID means a stale lock (e.g. after SIGKILL), which is reclaimed; a live
//! PID means another orchestrator is running. The lock is removed on drop.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ports::ProcessProbe;

/// Errors from acquiring the run lock.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another live orchestrator already holds the lock.
    #[error(
        "another orchestrator is already running (pid {pid}) → stop it, or wait for it to finish"
    )]
    AlreadyRunning {
        /// PID of the live holder.
        pid: u32,
    },
    /// Filesystem error while reading/writing the lock.
    #[error("run lock io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A held run lock. Deleting the file on drop releases it.
#[derive(Debug)]
pub struct RunLock {
    path: PathBuf,
}

impl RunLock {
    /// Acquire the lock at `path`, reclaiming it if the recorded PID is dead.
    pub fn acquire<P: ProcessProbe>(path: &Path, probe: &P) -> Result<Self, LockError> {
        if path.exists() {
            let contents = fs::read_to_string(path)?;
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if probe.is_alive(pid) {
                    return Err(LockError::AlreadyRunning { pid });
                }
                // Dead PID -> stale lock; fall through and reclaim it.
            }
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(path, std::process::id().to_string())?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        // Best-effort release; nothing actionable if it fails.
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe with a fixed answer, to simulate live vs dead holders.
    struct FixedProbe(bool);
    impl ProcessProbe for FixedProbe {
        fn is_alive(&self, _pid: u32) -> bool {
            self.0
        }
    }

    fn lock_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("run.lock")
    }

    #[test]
    fn acquires_and_releases_on_drop() {
        let path = lock_path("lock_basic");
        {
            let _lock = RunLock::acquire(&path, &FixedProbe(false)).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "lock file removed on drop");
    }

    #[test]
    fn rejects_when_holder_is_alive() {
        let path = lock_path("lock_alive");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "424242").unwrap();
        let err = RunLock::acquire(&path, &FixedProbe(true)).unwrap_err();
        assert!(matches!(err, LockError::AlreadyRunning { pid: 424242 }));
    }

    #[test]
    fn reclaims_stale_lock_from_dead_pid() {
        let path = lock_path("lock_stale");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "424242").unwrap();
        // Dead holder -> reclaimed; our PID is written.
        let _lock = RunLock::acquire(&path, &FixedProbe(false)).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written.trim(), std::process::id().to_string());
    }
}
