//! Errors from driving the orca CLI.

/// An error running or interpreting an `orca` CLI invocation.
#[derive(Debug, thiserror::Error)]
pub enum OrcaError {
    /// The `orca` binary could not be spawned (not installed / wrong path).
    #[error(
        "cannot run the orca CLI (`{bin}`) → is orca installed and on PATH? set `orca_bin` in plugins/orca.toml: {source}"
    )]
    Spawn {
        /// The binary we tried to run.
        bin: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// `orca` exited non-zero.
    #[error("orca exited with code {code}: {stderr}")]
    CliFailed {
        /// Process exit code (or -1 if terminated by signal).
        code: i32,
        /// Captured stderr (truncated).
        stderr: String,
    },
    /// The `--json` output could not be parsed.
    #[error("orca --json output was not valid JSON: {0}")]
    InvalidJson(String),
    /// The output was valid JSON but not the shape we expected.
    #[error("orca returned an unexpected response: {0}")]
    InvalidResponse(String),
    /// A referenced worktree was gone (for `session/attach`, F-37).
    #[error("{0}")]
    NotFound(String),
}

impl OrcaError {
    /// Whether this error means the referenced worktree no longer exists, so
    /// `session/attach` reports `attached: false` rather than failing (F-37).
    pub fn is_missing(&self) -> bool {
        match self {
            OrcaError::NotFound(_) => true,
            // orca signals a vanished worktree with a not-found message on
            // stderr. The exact wording is not contractual (orca is
            // daily-released), so match several phrasings best-effort.
            OrcaError::CliFailed { stderr, .. } => {
                let s = stderr.to_ascii_lowercase();
                s.contains("not found")
                    || s.contains("no such worktree")
                    || s.contains("no worktree")
                    || s.contains("unknown worktree")
                    || s.contains("does not exist")
                    || s.contains("invalid worktree")
            }
            _ => false,
        }
    }
}
