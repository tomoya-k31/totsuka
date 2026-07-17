//! Task state machine (F-71).
//!
//! The state machine is mode-agnostic: the concrete meaning of `Running`
//! (plan vs implement) and `Publishing` (PR vs source write-back) is decided
//! by the workflow (#54). Transitions are a pure function, [`transition`], so
//! they are trivially testable and never touch I/O.

use std::fmt;
use std::str::FromStr;

/// A task's lifecycle state (F-71).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Ingested, waiting for a free execution slot.
    Queued,
    /// Waiting for a human to confirm the selected repository (F-14).
    Pending,
    /// Handed to an agent, not yet running.
    Dispatched,
    /// Agent is working (plan or implement).
    Running,
    /// Agent is blocked on a human question (F-35). Frees its slot (F-45).
    WaitingInput,
    /// Agent self-reported COMPLETED; waiting for human verification
    /// (`totsuka task verify`, #131 D-01). Holds its slot like `Publishing`.
    Verifying,
    /// Escalated to a human (UNKNOWN stops / timeout / correlation anomaly,
    /// #131 D-02/D-03). Non-terminal: resolving it in the pane resumes the
    /// task on the next signal. Frees its slot like `WaitingInput`.
    Escalated,
    /// Producing output (PR creation or source write-back).
    Publishing,
    /// Completed successfully.
    Done,
    /// Ended in failure.
    Failed,
    /// Cancelled by a human.
    Cancelled,
}

impl TaskState {
    /// The state name persisted in SQLite (`tasks.state`, TEXT).
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Pending => "pending",
            TaskState::Dispatched => "dispatched",
            TaskState::Running => "running",
            TaskState::WaitingInput => "waiting_input",
            TaskState::Verifying => "verifying",
            TaskState::Escalated => "escalated",
            TaskState::Publishing => "publishing",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }

    /// Whether this is a terminal state (no further transitions except retry).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Cancelled
        )
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown state string from the DB.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown task state: {0:?}")]
pub struct UnknownState(pub String);

impl FromStr for TaskState {
    type Err = UnknownState;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "queued" => TaskState::Queued,
            "pending" => TaskState::Pending,
            "dispatched" => TaskState::Dispatched,
            "running" => TaskState::Running,
            "waiting_input" => TaskState::WaitingInput,
            "verifying" => TaskState::Verifying,
            "escalated" => TaskState::Escalated,
            "publishing" => TaskState::Publishing,
            "done" => TaskState::Done,
            "failed" => TaskState::Failed,
            "cancelled" => TaskState::Cancelled,
            other => return Err(UnknownState(other.to_string())),
        })
    }
}

/// An event that drives a state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEvent {
    /// Repository is ambiguous; ask a human to confirm (F-14).
    NeedRepoConfirmation,
    /// Human confirmed the repository; requeue.
    RepoConfirmed,
    /// Assign the task to an agent.
    Dispatch,
    /// Agent started working.
    Start,
    /// Agent is blocked on a human question (F-35).
    WaitInput,
    /// Human answered; resume work (F-44).
    ResumeInput,
    /// Begin producing output.
    BeginPublish,
    /// Agent self-reported COMPLETED under `verification = "human"` (#131
    /// D-01): move to `Verifying` and wait for `totsuka task verify`.
    SelfReportComplete,
    /// Human verification passed (`totsuka task verify --pass`).
    ApproveVerification,
    /// Human verification failed (`totsuka task verify --fail`); the human
    /// gives corrective instructions directly in the pane (D-07).
    VerificationFailed,
    /// Escalate to a human (UNKNOWN stops / timeout / correlation anomaly,
    /// #131 D-02/D-03).
    Escalate,
    /// Output produced successfully.
    Complete,
    /// Something failed.
    Fail,
    /// Human cancelled the task.
    Cancel,
    /// Retry a finished task (F-44): requeue from failed/cancelled.
    Retry,
}

/// An illegal `(state, event)` combination.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("illegal transition: cannot apply {event:?} in state {from}")]
pub struct InvalidTransition {
    /// The state the task was in.
    pub from: TaskState,
    /// The event that could not be applied.
    pub event: TaskEvent,
}

/// Compute the next state for `(from, event)`, or an error if illegal (F-71).
pub fn transition(from: TaskState, event: TaskEvent) -> Result<TaskState, InvalidTransition> {
    use TaskEvent as E;
    use TaskState as S;

    let to = match (from, event) {
        (S::Queued, E::NeedRepoConfirmation) => S::Pending,
        (S::Pending, E::RepoConfirmed) => S::Queued,
        (S::Queued, E::Dispatch) => S::Dispatched,
        (S::Dispatched, E::Start) => S::Running,
        (S::Running, E::WaitInput) => S::WaitingInput,
        (S::WaitingInput, E::ResumeInput) => S::Running,
        (S::Running, E::BeginPublish) => S::Publishing,
        // llm/none verification: COMPLETED may arrive while the task sits in
        // WaitingInput or Escalated (the human resolved it in the pane).
        (S::WaitingInput | S::Escalated, E::BeginPublish) => S::Publishing,
        // human verification (#131 D-01): COMPLETED self-report awaits
        // `totsuka task verify` instead of publishing directly.
        (S::Running | S::WaitingInput | S::Escalated, E::SelfReportComplete) => S::Verifying,
        (S::Verifying, E::ApproveVerification) => S::Publishing,
        (S::Verifying, E::VerificationFailed) => S::Running,
        // Escalated resumes to Running when the next signal reports plain
        // activity (Start-equivalent) or a question (WaitInput).
        (S::Escalated, E::Start) => S::Running,
        (S::Escalated, E::WaitInput) => S::WaitingInput,
        (S::Publishing, E::Complete) => S::Done,
        // Retry a terminal (non-Done) task: worktree/session handling is #57.
        (S::Failed | S::Cancelled, E::Retry) => S::Queued,
        // Escalation is reachable from any non-terminal state (#131 D-02).
        (s, E::Escalate) if !s.is_terminal() => S::Escalated,
        // Failure is reachable from any non-terminal state.
        (s, E::Fail) if !s.is_terminal() => S::Failed,
        // Cancellation is reachable from any non-terminal state.
        (s, E::Cancel) if !s.is_terminal() => S::Cancelled,
        _ => return Err(InvalidTransition { from, event }),
    };
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_queued_to_done() {
        let mut s = TaskState::Queued;
        for (event, expected) in [
            (TaskEvent::Dispatch, TaskState::Dispatched),
            (TaskEvent::Start, TaskState::Running),
            (TaskEvent::BeginPublish, TaskState::Publishing),
            (TaskEvent::Complete, TaskState::Done),
        ] {
            s = transition(s, event).unwrap();
            assert_eq!(s, expected);
        }
        assert!(s.is_terminal());
    }

    #[test]
    fn waiting_input_round_trip() {
        let s = transition(TaskState::Running, TaskEvent::WaitInput).unwrap();
        assert_eq!(s, TaskState::WaitingInput);
        let s = transition(s, TaskEvent::ResumeInput).unwrap();
        assert_eq!(s, TaskState::Running);
    }

    #[test]
    fn pending_confirmation_round_trip() {
        let s = transition(TaskState::Queued, TaskEvent::NeedRepoConfirmation).unwrap();
        assert_eq!(s, TaskState::Pending);
        assert_eq!(
            transition(s, TaskEvent::RepoConfirmed).unwrap(),
            TaskState::Queued
        );
    }

    /// Every non-terminal state (kept in sync with the enum).
    const NON_TERMINAL: [TaskState; 8] = [
        TaskState::Queued,
        TaskState::Pending,
        TaskState::Dispatched,
        TaskState::Running,
        TaskState::WaitingInput,
        TaskState::Verifying,
        TaskState::Escalated,
        TaskState::Publishing,
    ];

    #[test]
    fn fail_and_cancel_from_active_states() {
        // Explicitly includes Verifying and Escalated: the catch-all Fail /
        // Cancel arms must keep reaching the new non-terminal states.
        for from in NON_TERMINAL {
            assert_eq!(
                transition(from, TaskEvent::Fail).unwrap(),
                TaskState::Failed
            );
            assert_eq!(
                transition(from, TaskEvent::Cancel).unwrap(),
                TaskState::Cancelled
            );
        }
    }

    #[test]
    fn retry_requeues_terminal_failures() {
        assert_eq!(
            transition(TaskState::Failed, TaskEvent::Retry).unwrap(),
            TaskState::Queued
        );
        assert_eq!(
            transition(TaskState::Cancelled, TaskEvent::Retry).unwrap(),
            TaskState::Queued
        );
    }

    #[test]
    fn human_verification_round_trip() {
        // COMPLETED self-report -> Verifying (from every announcing state).
        for from in [
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Escalated,
        ] {
            assert_eq!(
                transition(from, TaskEvent::SelfReportComplete).unwrap(),
                TaskState::Verifying
            );
        }
        // `totsuka task verify --pass` publishes; `--fail` goes back to work.
        assert_eq!(
            transition(TaskState::Verifying, TaskEvent::ApproveVerification).unwrap(),
            TaskState::Publishing
        );
        assert_eq!(
            transition(TaskState::Verifying, TaskEvent::VerificationFailed).unwrap(),
            TaskState::Running
        );
    }

    #[test]
    fn escalate_reaches_every_non_terminal_state() {
        for from in NON_TERMINAL {
            assert_eq!(
                transition(from, TaskEvent::Escalate).unwrap(),
                TaskState::Escalated,
                "Escalate from {from}"
            );
        }
        for from in [TaskState::Done, TaskState::Failed, TaskState::Cancelled] {
            assert!(transition(from, TaskEvent::Escalate).is_err());
        }
    }

    #[test]
    fn escalated_recovers_on_every_path() {
        // Human resolves the pane; the next signal carries the resume path.
        for (event, expected) in [
            (TaskEvent::SelfReportComplete, TaskState::Verifying), // human verify
            (TaskEvent::BeginPublish, TaskState::Publishing),      // llm/none verify
            (TaskEvent::WaitInput, TaskState::WaitingInput),
            (TaskEvent::Start, TaskState::Running),
        ] {
            assert_eq!(
                transition(TaskState::Escalated, event).unwrap(),
                expected,
                "Escalated + {event:?}"
            );
        }
    }

    #[test]
    fn begin_publish_from_waiting_input() {
        // llm/none COMPLETED arriving while the task waits for input.
        assert_eq!(
            transition(TaskState::WaitingInput, TaskEvent::BeginPublish).unwrap(),
            TaskState::Publishing
        );
    }

    #[test]
    fn new_states_are_not_terminal() {
        assert!(!TaskState::Verifying.is_terminal());
        assert!(!TaskState::Escalated.is_terminal());
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        // Cannot start a queued task without dispatching first.
        assert!(transition(TaskState::Queued, TaskEvent::Start).is_err());
        // Cannot fail/cancel a terminal task.
        assert!(transition(TaskState::Done, TaskEvent::Fail).is_err());
        assert!(transition(TaskState::Done, TaskEvent::Cancel).is_err());
        // Cannot retry a successful task.
        assert!(transition(TaskState::Done, TaskEvent::Retry).is_err());
        // Cannot publish straight from dispatched.
        assert!(transition(TaskState::Dispatched, TaskEvent::BeginPublish).is_err());
        // Verification events only apply to the states they belong to.
        assert!(transition(TaskState::Running, TaskEvent::ApproveVerification).is_err());
        assert!(transition(TaskState::Running, TaskEvent::VerificationFailed).is_err());
        assert!(transition(TaskState::Queued, TaskEvent::SelfReportComplete).is_err());
        assert!(transition(TaskState::Verifying, TaskEvent::SelfReportComplete).is_err());
    }

    #[test]
    fn state_string_round_trips() {
        for s in [
            TaskState::Queued,
            TaskState::Pending,
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Verifying,
            TaskState::Escalated,
            TaskState::Publishing,
            TaskState::Done,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            assert_eq!(s.as_str().parse::<TaskState>().unwrap(), s);
        }
        assert!("bogus".parse::<TaskState>().is_err());
    }
}
