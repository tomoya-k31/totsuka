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
        (S::Publishing, E::Complete) => S::Done,
        // Retry a terminal (non-Done) task: worktree/session handling is #57.
        (S::Failed | S::Cancelled, E::Retry) => S::Queued,
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

    #[test]
    fn fail_and_cancel_from_active_states() {
        for from in [
            TaskState::Queued,
            TaskState::Pending,
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Publishing,
        ] {
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
    }

    #[test]
    fn state_string_round_trips() {
        for s in [
            TaskState::Queued,
            TaskState::Pending,
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::WaitingInput,
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
