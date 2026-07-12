//! Errors from delivering a macOS notification.

/// An error sending a notification via the configured backend.
#[derive(Debug, thiserror::Error)]
pub enum NotifierError {
    /// The notification tool (`osascript`) could not be spawned.
    #[error("cannot run `{bin}` to post a notification: {source}")]
    Spawn {
        /// The binary we tried to run.
        bin: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The tool ran but exited non-zero.
    #[error("`{bin}` exited with code {code}: {stderr}")]
    Failed {
        /// The binary.
        bin: String,
        /// Process exit code (or -1 if terminated by signal).
        code: i32,
        /// Captured stderr (truncated).
        stderr: String,
    },
}
