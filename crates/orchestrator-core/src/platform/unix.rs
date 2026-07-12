//! POSIX process liveness via `kill(pid, 0)`.
//!
//! `kill` with signal `0` performs error checking without sending a signal:
//! it returns `0` when the process exists, and fails with `EPERM` when the
//! process exists but we lack permission to signal it. Both mean "alive".

use crate::ports::ProcessProbe;

/// [`ProcessProbe`] backed by `kill(pid, 0)`. Available on all Unix targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnixProcessProbe;

impl ProcessProbe for UnixProcessProbe {
    fn is_alive(&self, pid: u32) -> bool {
        // SAFETY: `kill` with signal 0 has no side effects; it only reports
        // whether `pid` can be signalled.
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM means the process exists but is owned by another user.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(UnixProcessProbe.is_alive(pid));
    }

    #[test]
    fn pid_one_is_alive() {
        // PID 1 (init/launchd) always exists; probing it exercises the EPERM
        // branch on systems where we cannot signal it.
        assert!(UnixProcessProbe.is_alive(1));
    }

    #[test]
    fn almost_certainly_dead_pid_is_not_alive() {
        // A very high PID is exceedingly unlikely to be in use.
        assert!(!UnixProcessProbe.is_alive(u32::MAX - 1));
    }
}
