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
    /// A dispatch that asked to resume a session died with its pane, so the
    /// session could not be resumed (protocol `SESSION_UNRESUMABLE`, #242).
    /// Carries the herdr error underneath, which is what a human debugging it
    /// needs.
    #[error("the agent session could not be resumed: {0}")]
    SessionUnresumable(String),
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

    /// Whether this is `agent.start` refusing a pane that has not reached its
    /// interactive shell prompt yet — a **transient** state that clears on its
    /// own within seconds.
    ///
    /// A freshly created workspace's root pane is still starting its shell, and
    /// protocol 17 hands `agent.start` that pane directly. Measured live: a
    /// dispatch calling `agent.start` about a second after `workspace.create`
    /// got `agent_pane_busy: agent target pane w5:p1 is not an available
    /// shell`, while the same call a few seconds later succeeded. How long the
    /// window lasts is the operator's shell startup, so it is waited out rather
    /// than predicted.
    pub fn is_pane_not_ready(&self) -> bool {
        matches!(self, HerdrError::Protocol { code, .. } if code == "agent_pane_busy")
    }

    /// Whether this is herdr refusing to address an agent that `agent.start`
    /// accepted but has not finished launching — also **transient**.
    ///
    /// A successful `agent.start` can answer `launch_pending: true` with
    /// `agent_status: unknown`, which means "the launch was accepted", not
    /// "the agent can take a prompt". Measured live: the start succeeded at
    /// t=1.0s and `agent.prompt` answered `agent_not_ready` until t=5.0s.
    ///
    /// Deliberately **not** `agent_not_found`. That one is
    /// [`is_missing`](Self::is_missing) — the shape a pane that died takes —
    /// and on a resumed dispatch it means the session is unresumable (#261).
    /// Retrying it would delay a real failure and, worse, keep a
    /// `SESSION_UNRESUMABLE` from being reported as one.
    pub fn is_agent_not_ready(&self) -> bool {
        matches!(self, HerdrError::Protocol { code, .. } if code == "agent_not_ready")
    }
}
