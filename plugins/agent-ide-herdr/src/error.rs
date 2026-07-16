//! Errors from talking to the herdr Socket API.

/// An error connecting to, or communicating with, herdr.
#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    /// The Unix socket could not be reached (herdr not running / wrong path).
    #[error(
        "cannot connect to herdr socket at {path} → is herdr running? check `socket_path`/`session` in plugins/herdr.toml (or HERDR_SOCKET_PATH): {source}"
    )]
    Connect {
        /// The socket path we tried.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A read/write failure on an established connection.
    #[error("herdr socket I/O error: {0}")]
    Io(String),
    /// The request timed out waiting for a response.
    #[error("herdr request `{0}` timed out")]
    Timeout(String),
    /// herdr returned an `error` object for a request.
    #[error("herdr error ({code}): {message}")]
    Protocol {
        /// herdr error code (e.g. `not_found`).
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// A referenced pane/session was gone (`pane not found`), used for
    /// `session/attach` failure (F-37).
    #[error("{0}")]
    NotFound(String),
    /// The response was not the shape we expected.
    #[error("herdr returned an unexpected response: {0}")]
    InvalidResponse(String),
}

impl HerdrError {
    /// Whether this error means the referenced pane/session no longer exists,
    /// so `session/attach` should report `attached: false` rather than fail
    /// (F-37; the Orchestrator's recovery, #57, then defers to a human).
    pub fn is_missing(&self) -> bool {
        match self {
            HerdrError::NotFound(_) => true,
            // herdr scopes its not-found codes per target (`pane_not_found`,
            // `agent_not_found`, …); older docs used a bare `not_found`.
            HerdrError::Protocol { code, .. } => {
                code == "not_found" || code.ends_with("_not_found")
            }
            _ => false,
        }
    }
}
