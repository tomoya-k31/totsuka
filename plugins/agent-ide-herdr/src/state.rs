//! State mapping and the dispatch session handle (F-32/F-37).
//!
//! herdr's `agent_status` and pane-exit signals are normalized to totsuka's
//! [`AgentState`]; the `(pane_id, agent_session_id)` re-attach handle (F-37) is
//! encoded into the single protocol `session_id` string; and a `blocked`
//! agent's question is best-effort extracted from pane scrollback (F-35).

use plugin_protocol::methods::AgentState;

/// Map a herdr `agent_status` to the totsuka normalized state (F-32).
///
/// `unknown` has no totsuka equivalent, so the previous state is retained
/// (a degraded hold rather than a spurious transition). herdr has no `failed`
/// status — that is derived from a non-zero pane exit (see [`state_from_exit`]).
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

/// Derive a totsuka state from a `pane.exited` event (F-32): a clean exit is
/// `done`, a non-zero exit is `failed` (herdr has no native `failed`).
pub fn state_from_exit(exit_code: i64) -> AgentState {
    if exit_code == 0 {
        AgentState::Done
    } else {
        AgentState::Failed
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

/// Best-effort extraction of a `blocked` agent's question from pane scrollback
/// (F-35). Claude Code emits no structured "waiting for input" signal, so this
/// takes the trailing prompt text: the lines after the last blank line, trimmed.
/// Returns `None` when scrollback is empty.
pub fn extract_question(scrollback: &str) -> Option<String> {
    let trimmed = scrollback.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    // Take the last "paragraph" (run of non-blank lines) as the prompt.
    let tail: Vec<&str> = trimmed
        .lines()
        .rev()
        .take_while(|line| !line.trim().is_empty())
        .collect();
    let question = tail
        .into_iter()
        .rev()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let question = question.trim().to_string();
    (!question.is_empty()).then_some(question)
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
    fn failed_comes_from_nonzero_exit_only() {
        assert_eq!(state_from_exit(0), AgentState::Done);
        assert_eq!(state_from_exit(1), AgentState::Failed);
        assert_eq!(state_from_exit(130), AgentState::Failed);
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

    #[test]
    fn extracts_trailing_question_from_scrollback() {
        let scrollback =
            "Running tests...\nAll good.\n\nShould I delete the old config file? (y/n)\n";
        assert_eq!(
            extract_question(scrollback).as_deref(),
            Some("Should I delete the old config file? (y/n)")
        );
    }

    #[test]
    fn extracts_multiline_trailing_prompt() {
        let scrollback =
            "context above\n\nI found two approaches:\n1. rewrite\n2. patch\nWhich do you prefer?";
        assert_eq!(
            extract_question(scrollback).as_deref(),
            Some("I found two approaches:\n1. rewrite\n2. patch\nWhich do you prefer?")
        );
    }

    #[test]
    fn empty_scrollback_yields_no_question() {
        assert_eq!(extract_question(""), None);
        assert_eq!(extract_question("   \n  \n"), None);
    }
}
