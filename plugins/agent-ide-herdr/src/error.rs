//! Errors from talking to the herdr Socket API.

/// An error connecting to, or communicating with, herdr.
#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    /// The Unix socket could not be reached (herdr not running / wrong path).
    #[error(
        "cannot connect to herdr socket at {path} → is herdr running? check `socket_path`/`session` in `[herdr]` of config.toml (or HERDR_SOCKET_PATH): {source}"
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
    /// `task/dispatch` arrived without a `tool_launch` (#411). Not a herdr
    /// failure at all — the Orchestrator failed to resolve the agent argv —
    /// so it maps to `INVALID_PARAMS`, not the internal-error catch-all.
    #[error(
        "task/dispatch carried no `tool_launch`: since protocol 0.4.0 (#411) this plugin has no \
         local argv fallback, so there is nothing to launch. This is an Orchestrator-side tool \
         resolution failure — check `[tools]` / `default_tool`."
    )]
    MissingToolLaunch,
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

    /// Whether this is a request to `method` timing out — the shape the
    /// shell-readiness race takes when herdr waits for the CLI instead of
    /// refusing the pane outright (#387).
    ///
    /// **`method` has to be supplied by the caller.** herdr's `timeout` is a
    /// generic code carried in a [`Protocol`](Self::Protocol) payload that
    /// names no method, and other calls answer it too — `agent.wait` most
    /// obviously. Matching the code alone would therefore claim more than the
    /// value knows, so the call site passes the method it just made and this
    /// only speaks for that one.
    ///
    /// Measured live (#387): calling `agent.start` immediately after
    /// `workspace.create` answered `timeout: timed out waiting for agent
    /// startup` while the pane stayed **completely empty** — the launch
    /// command had been typed into a shell that was not accepting input yet,
    /// so the keystrokes were swallowed. Waiting does not undo that: the same
    /// call with a 120s window still failed, and the pane was still empty
    /// after all 120s. Re-issuing `agent.start` on that pane succeeded in ~3s.
    ///
    /// Also covers the client-side [`Timeout`](Self::Timeout), which *does*
    /// carry the method: `request_timeout_secs` can expire before herdr
    /// answers, and that is the same "the shell was not ready" story seen from
    /// this side of the socket.
    pub fn is_timeout_of(&self, method: &str) -> bool {
        match self {
            HerdrError::Protocol { code, .. } => code == "timeout",
            HerdrError::Timeout(timed_out) => timed_out == method,
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

    /// Whether this is herdr saying the *named agent* does not exist.
    ///
    /// A deliberate narrowing of [`is_missing`](Self::is_missing), which also
    /// matches the pane-level codes. What to do about the two differs: a pane
    /// that is gone cannot be started into, so re-issuing `agent.start` would
    /// only fail again, while an agent that is gone from a pane that is still
    /// there is the shell-readiness race (#391) and a re-issue clears it.
    pub fn is_agent_missing(&self) -> bool {
        matches!(self, HerdrError::Protocol { code, .. } if code == "agent_not_found")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol(code: &str) -> HerdrError {
        HerdrError::Protocol {
            code: code.to_string(),
            message: String::new(),
        }
    }

    /// The client-side timeout names the method it gave up on, so it answers
    /// only for that one — asking about a different call must not match.
    #[test]
    fn a_client_timeout_speaks_only_for_its_own_method() {
        let e = HerdrError::Timeout("agent.start".to_string());
        assert!(e.is_timeout_of("agent.start"));
        assert!(!e.is_timeout_of("agent.wait"));
    }

    /// herdr's `timeout` payload names no method, which is exactly why the
    /// caller has to supply one: the code alone cannot tell an `agent.start`
    /// that never saw the CLI from an `agent.wait` that ran out its window.
    /// Pinning that here so the asymmetry with the client-side arm is
    /// deliberate rather than discovered.
    #[test]
    fn a_protocol_timeout_carries_no_method_so_the_call_site_scopes_it() {
        assert!(protocol("timeout").is_timeout_of("agent.start"));
        assert!(protocol("timeout").is_timeout_of("agent.wait"));
    }

    /// Only `timeout` is the readiness race. The refusals that mean "this will
    /// not fix itself" must not be swept in.
    #[test]
    fn other_codes_are_not_timeouts() {
        for code in ["agent_name_taken", "agent_not_ready", "agent_pane_busy"] {
            assert!(
                !protocol(code).is_timeout_of("agent.start"),
                "{code} must not read as a timeout"
            );
        }
    }
}
