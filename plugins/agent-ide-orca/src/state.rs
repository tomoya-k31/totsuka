//! State normalization (F-32) and question extraction (F-35).
//!
//! orca reports a coarse 3-value agent state derived from OSC "state dots" (a
//! status-line hook), surfaced by `orca worktree ps`. It is mapped to totsuka's
//! [`AgentState`]. There is no native `failed`; that is derived from an abnormal
//! terminal state. A blocked agent's question is best-effort extracted from the
//! terminal read (orca has no structured question signal).

use plugin_protocol::methods::AgentState;

/// Map an orca worktree `state` string to the totsuka normalized state (F-32).
///
/// The three state-dot values are `working`/`waiting`/`done` (with `idle`
/// meaning tui-idle = turn complete). Abnormal states (`error`/`crashed`/…) map
/// to `failed` (orca has no native `failed`). Anything unrecognized holds the
/// previous state rather than inventing a transition.
pub fn map_orca_state(state: &str, previous: AgentState) -> AgentState {
    match state.to_ascii_lowercase().as_str() {
        "working" | "running" | "busy" => AgentState::Running,
        // A `waiting`/`blocked` dot is an approval/input stop → waiting_input.
        // This is what distinguishes it from a plain tui-idle "done" (the
        // "approval-waiting idle" ambiguity the reference warns about).
        "waiting" | "blocked" | "input" | "paused" => AgentState::WaitingInput,
        "done" | "idle" | "tui-idle" | "completed" | "finished" => AgentState::Done,
        "error" | "failed" | "crashed" | "exited" | "timeout" => AgentState::Failed,
        _ => previous,
    }
}

/// Best-effort extraction of a blocked agent's question from a terminal read
/// (F-35): the trailing run of non-blank lines. `None` when empty. orca surfaces
/// no structured question, so this is a heuristic on the terminal output.
pub fn extract_question(terminal_output: &str) -> Option<String> {
    let trimmed = terminal_output.trim_end();
    if trimmed.is_empty() {
        return None;
    }
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
    fn maps_the_three_state_dots_and_failure() {
        let prev = AgentState::Idle;
        assert_eq!(map_orca_state("working", prev), AgentState::Running);
        assert_eq!(map_orca_state("waiting", prev), AgentState::WaitingInput);
        assert_eq!(map_orca_state("done", prev), AgentState::Done);
        // tui-idle counts as done; abnormal exit is the only `failed` source.
        assert_eq!(map_orca_state("idle", prev), AgentState::Done);
        assert_eq!(map_orca_state("crashed", prev), AgentState::Failed);
        assert_eq!(map_orca_state("timeout", prev), AgentState::Failed);
    }

    #[test]
    fn mapping_is_case_insensitive_and_holds_unknown() {
        assert_eq!(
            map_orca_state("WORKING", AgentState::Idle),
            AgentState::Running
        );
        // Unknown state holds the previous rather than inventing a transition.
        assert_eq!(
            map_orca_state("???", AgentState::Running),
            AgentState::Running
        );
    }

    #[test]
    fn extracts_trailing_question() {
        let out = "running build...\ndone.\n\nApply these changes? [y/N]\n";
        assert_eq!(
            extract_question(out).as_deref(),
            Some("Apply these changes? [y/N]")
        );
        assert_eq!(extract_question("   \n"), None);
    }
}
