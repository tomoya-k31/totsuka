//! State normalization for `session/attach` (F-32/F-37).
//!
//! orca reports a coarse per-worktree agent status (`worktree ps`'s `status`,
//! derived from the agent's OSC "state dots"). It is mapped to totsuka's
//! [`AgentState`] only to tell recovery what a re-attached agent is doing.
//! Task **completion** does not flow through here: it is reported by the
//! agent's hooks (`hook_completion`), and the state stream is an exit deadman
//! (see `agent::OrcaAgent::start_state_stream`) — the same split as the herdr
//! plugin (#131).

use plugin_protocol::methods::AgentState;

/// Map an orca worktree `status` to the totsuka normalized state (F-32).
///
/// Anything unrecognized — including orca's `active` (a worktree with a live
/// terminal and no agent signal) — holds `previous` rather than inventing a
/// transition: the caller has already established that the agent's terminal is
/// alive, and passes the state that fact implies.
pub fn map_orca_state(status: &str, previous: AgentState) -> AgentState {
    match status.to_ascii_lowercase().as_str() {
        "working" | "running" | "busy" => AgentState::Running,
        // An approval / input stop.
        "waiting" | "blocked" | "input" | "permission" => AgentState::WaitingInput,
        "done" | "completed" | "finished" => AgentState::Done,
        "idle" => AgentState::Idle,
        "error" | "failed" | "crashed" => AgentState::Failed,
        _ => previous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_state_dots() {
        let prev = AgentState::Running;
        assert_eq!(map_orca_state("working", prev), AgentState::Running);
        assert_eq!(map_orca_state("waiting", prev), AgentState::WaitingInput);
        assert_eq!(map_orca_state("done", prev), AgentState::Done);
        assert_eq!(map_orca_state("idle", prev), AgentState::Idle);
        assert_eq!(map_orca_state("crashed", prev), AgentState::Failed);
    }

    #[test]
    fn mapping_is_case_insensitive_and_holds_unknown() {
        assert_eq!(
            map_orca_state("WORKING", AgentState::Idle),
            AgentState::Running
        );
        // `active` is orca's "has a terminal" — not an agent state.
        assert_eq!(
            map_orca_state("active", AgentState::Running),
            AgentState::Running
        );
        assert_eq!(
            map_orca_state("???", AgentState::WaitingInput),
            AgentState::WaitingInput
        );
    }
}
