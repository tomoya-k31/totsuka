//! Process liveness port ([`ProcessProbe`]).
//!
//! Used by `run`'s single-instance guard to tell a live orchestrator from a
//! stale lock file (F-74).

/// Checks whether a process is currently alive.
pub trait ProcessProbe {
    /// Return `true` if a process with `pid` currently exists.
    fn is_alive(&self, pid: u32) -> bool;
}
