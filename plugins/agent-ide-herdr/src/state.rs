//! State mapping, the dispatch session handle (F-32/F-37), and the screen-text
//! helper the adapter needs.
//!
//! herdr's `agent_status` is normalized to totsuka's [`AgentState`] for
//! `session/attach`, and the `(pane_id, agent_session_id)` re-attach handle
//! (F-37) is encoded into the single protocol `session_id` string.

use plugin_protocol::methods::AgentState;

/// Map a herdr `agent_status` to the totsuka normalized state (F-32).
///
/// `unknown` has no totsuka equivalent, so the previous state is retained
/// (a degraded hold rather than a spurious transition). herdr has no `failed`
/// status. Used by `session/attach` to report a re-attached pane's current
/// state; task **completion** no longer flows through here — it is reported by
/// Claude Code's hooks (#131), and the state stream is a `pane.exited` deadman
/// (see `agent::start_state_stream`).
pub fn map_agent_status(status: &str, previous: AgentState) -> AgentState {
    match status {
        "idle" => AgentState::Idle,
        "working" => AgentState::Running,
        "blocked" => AgentState::WaitingInput,
        "done" => AgentState::Done,
        // `unknown` (and any status herdr adds later) holds the last known state.
        _ => previous,
    }
}

/// The re-attach handle for a dispatched task (F-37): the herdr pane running the
/// agent and the agent's own conversation/session id (for `claude --resume`).
///
/// It is encoded into the protocol's single `session_id` string as
/// `"<pane_id>|<agent_session_id>"`. `pane_id` is a herdr id like `w1:p1`
/// (never contains `|`), so the first `|` is an unambiguous separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandle {
    /// herdr pane id (e.g. `w1:p1`).
    pub pane_id: String,
    /// The agent's native session id, or empty if not yet detected.
    pub agent_session_id: String,
}

impl SessionHandle {
    /// Build a handle.
    pub fn new(pane_id: impl Into<String>, agent_session_id: impl Into<String>) -> Self {
        Self {
            pane_id: pane_id.into(),
            agent_session_id: agent_session_id.into(),
        }
    }

    /// Encode into the protocol `session_id` string.
    pub fn encode(&self) -> String {
        format!("{}|{}", self.pane_id, self.agent_session_id)
    }

    /// Decode a `session_id` string back into a handle. A string without a `|`
    /// is treated as a bare pane id (empty agent session), so a hand-written id
    /// still resolves a pane.
    pub fn decode(session_id: &str) -> Self {
        match session_id.split_once('|') {
            Some((pane, agent)) => Self::new(pane, agent),
            None => Self::new(session_id, ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_agent_status() {
        let prev = AgentState::Running;
        assert_eq!(map_agent_status("idle", prev), AgentState::Idle);
        assert_eq!(map_agent_status("working", prev), AgentState::Running);
        assert_eq!(map_agent_status("blocked", prev), AgentState::WaitingInput);
        assert_eq!(map_agent_status("done", prev), AgentState::Done);
        // unknown holds the previous state, not a spurious transition.
        assert_eq!(map_agent_status("unknown", prev), AgentState::Running);
        assert_eq!(
            map_agent_status("unknown", AgentState::Idle),
            AgentState::Idle
        );
    }

    #[test]
    fn session_handle_round_trips() {
        let h = SessionHandle::new("w1:p1", "agent-abc-123");
        assert_eq!(h.encode(), "w1:p1|agent-abc-123");
        assert_eq!(SessionHandle::decode(&h.encode()), h);
        // A bare pane id (no separator) decodes with an empty agent session.
        assert_eq!(
            SessionHandle::decode("w2:p3"),
            SessionHandle::new("w2:p3", "")
        );
        // An empty agent session still round-trips.
        let bare = SessionHandle::new("w1:p1", "");
        assert_eq!(SessionHandle::decode(&bare.encode()), bare);
    }
}
