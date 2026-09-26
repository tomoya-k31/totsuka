//! POSIX process liveness via `kill(pid, 0)`.
//!
//! `kill` with signal `0` performs error checking without sending a signal:
//! it returns `0` when the process exists, and fails with `EPERM` when the
//! process exists but we lack permission to signal it. Both mean "alive".
//!
//! Also home to [`hostname`], which picks the per-host config file (#832).

use crate::ports::ProcessProbe;

/// [`ProcessProbe`] backed by `kill(pid, 0)`. Available on all Unix targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnixProcessProbe;

impl ProcessProbe for UnixProcessProbe {
    fn is_alive(&self, pid: u32) -> bool {
        // `pid_t` is signed. A `u32` that doesn't fit cannot name a real
        // process, and reinterpreting it as a negative value would make
        // `kill()` target a process group / broadcast instead — a false
        // positive. Reject out-of-range PIDs up front.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: `kill` with signal 0 has no side effects; it only reports
        // whether `pid` can be signalled.
        let ret = unsafe { libc::kill(pid, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM means the process exists but is owned by another user.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// This machine's hostname from `gethostname(2)`, or `None` when the call
/// fails or the name is not UTF-8.
pub fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for `buf.len()` bytes; the last byte is never
    // written, so the result stays NUL-terminated even on truncation.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
    if ret != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    String::from_utf8(buf[..end].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_is_available() {
        assert!(hostname().is_some_and(|h| !h.is_empty()));
    }

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
        // High but still within `pid_t` (i32) range, so this genuinely
        // exercises the `kill` path (returns ESRCH), not the guard below.
        assert!(!UnixProcessProbe.is_alive(2_000_000_000));
    }

    #[test]
    fn out_of_range_pid_is_not_alive() {
        // Beyond `pid_t::MAX`; must be rejected before reaching `kill` so it
        // never wraps to a negative process-group target.
        assert!(!UnixProcessProbe.is_alive(u32::MAX));
    }
}
