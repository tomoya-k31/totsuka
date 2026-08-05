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

    /// Whether this is `agent.start` giving up on a pane whose shell never
    /// reached its prompt — the **same race** as
    /// [`is_pane_not_ready`](Self::is_pane_not_ready), in the shape it takes
    /// when herdr waits for the CLI instead of refusing the pane outright.
    ///
    /// Measured live (#387): calling `agent.start` immediately after
    /// `workspace.create` answered `timeout: timed out waiting for agent
    /// startup` while the pane stayed **completely empty** — the launch
    /// command had been typed into a shell that was not accepting input yet,
    /// so the keystrokes were swallowed. Waiting does not undo that: the same
    /// call with a 120s window still failed, and the pane was still empty
    /// after all 120s. Re-issuing `agent.start` on that pane succeeded in ~3s.
    ///
    /// Also covers the client-side [`Timeout`](Self::Timeout) for that method:
    /// `request_timeout_secs` can expire before herdr answers, and that is the
    /// same "the shell was not ready" story seen from this side of the socket.
    pub fn is_agent_start_timeout(&self) -> bool {
        match self {
            HerdrError::Protocol { code, .. } => code == "timeout",
            HerdrError::Timeout(method) => method == "agent.start",
            _ => false,
        }
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

    /// Whether this is `agent.prompt` reporting that it saw no state change
    /// within herdr's own 5s floor.
    ///
    /// **Unlike the other two startup transients, the prompt has already been
    /// typed and submitted when this comes back.** herdr requires an observed
    /// state change within 5000ms of a submission from a non-working state, and
    /// that floor is not something the caller can raise — `wait.timeout_ms` is
    /// the outer bound on reaching a settled state, not this. A Claude Code that
    /// takes longer than 5s to visibly react therefore fails a dispatch that
    /// actually worked (observed on 3 of 7 live tasks, #380).
    ///
    /// So this must **never** be answered by re-sending: that would put the
    /// task into the agent twice. It is confirmed instead.
    pub fn is_prompt_stalled(&self) -> bool {
        matches!(self, HerdrError::Protocol { code, .. } if code == "agent_prompt_stalled")
    }
}
