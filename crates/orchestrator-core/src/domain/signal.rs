//! Hook signal domain types (#131, F-xx[hook]).
//!
//! Claude Code hooks (Stop / Notification / SessionStart / SessionEnd) POST
//! their JSON to the orchestrator over UDS; the driving adapter (#136)
//! normalizes each request into an [`AgentSignal`] before it reaches the
//! engine (#138). These are pure domain types: no I/O, no transport concerns.

use std::fmt;
use std::str::FromStr;

/// A normalized hook signal from an agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSignal {
    /// Where the signal came from.
    pub source: SignalSource,
    /// The dispatch this signal belongs to (`TOOL_A_JOB_ID`, E-09: never
    /// guessed from a session id alone).
    pub job_id: JobId,
    /// The hook input's `session_id` (may be empty).
    pub claude_session_id: String,
    /// The hook input's `prompt_id`, an idempotency-key component (may be
    /// empty).
    pub prompt_id: String,
    /// What happened.
    pub event: SignalEvent,
    /// The full received JSON, kept verbatim for the audit trail (N-01).
    pub payload: serde_json::Value,
}

/// The mechanism that produced a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalSource {
    /// A Claude Code hook (Stop / Notification / SessionStart / SessionEnd).
    ClaudeHook,
    // Future: HeadlessWrapper, ...
}

/// The event a signal carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalEvent {
    /// The agent stopped and self-reported an outcome via the status marker
    /// (`<<STATUS:...>>`, #131 D-12).
    Stop {
        /// The self-reported outcome.
        status: StopStatus,
        /// The marker's `reason="..."`, if any.
        reason: Option<String>,
        /// The hook input's `last_assistant_message`, if present.
        last_assistant_message: Option<String>,
        /// The hook input's `transcript_path`, if present.
        transcript_path: Option<String>,
    },
    /// A Notification hook fired (e.g. the agent asked for permission).
    Notification {
        /// The notification message, if present.
        message: Option<String>,
    },
    /// A session started (also carries the fresh session id for correlation).
    SessionStart {
        /// The new session's id.
        claude_session_id: String,
    },
    /// A session ended.
    SessionEnd {
        /// The hook input's end reason, if present.
        reason: Option<String>,
    },
    /// An intermediate Stop while `background_tasks` is non-empty: the agent
    /// is still working, so this only proves liveness (#131 D-12).
    Heartbeat,
}

/// The outcome an agent self-reports in a [`SignalEvent::Stop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStatus {
    /// `<<STATUS:COMPLETED>>`.
    Completed,
    /// `<<STATUS:NEEDS_INPUT ...>>`.
    NeedsInput,
    /// `<<STATUS:FAILED ...>>`.
    Failed,
    /// Marker missing or unparseable (`stop_hook_active` re-stop).
    Unknown,
}

/// Identifies one dispatch of one task: `"job-{task_id}-{session_row}"`.
///
/// Injected into the agent process as `TOOL_A_JOB_ID` and echoed back by
/// every hook, so a signal is correlated to its task without guessing from
/// session ids (E-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId {
    /// The task's id (`tasks.id`).
    pub task_id: i64,
    /// The dispatch's session row id (`sessions.id`).
    pub session_row: i64,
}

impl JobId {
    /// Build a job id for a task's dispatch.
    pub fn new(task_id: i64, session_row: i64) -> Self {
        Self {
            task_id,
            session_row,
        }
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job-{}-{}", self.task_id, self.session_row)
    }
}

/// Error returned when parsing a malformed job id string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid job id: {0:?} (expected \"job-{{task_id}}-{{session_row}}\")")]
pub struct InvalidJobId(pub String);

impl FromStr for JobId {
    type Err = InvalidJobId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidJobId(s.to_string());
        let rest = s.strip_prefix("job-").ok_or_else(invalid)?;
        let (task_id, session_row) = rest.split_once('-').ok_or_else(invalid)?;
        // Both parts are plain non-negative decimals; anything else (empty,
        // signs, extra separators) is rejected rather than guessed at.
        if task_id.is_empty()
            || session_row.is_empty()
            || !task_id.bytes().all(|b| b.is_ascii_digit())
            || !session_row.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let task_id = task_id.parse().map_err(|_| invalid())?;
        let session_row = session_row.parse().map_err(|_| invalid())?;
        Ok(Self {
            task_id,
            session_row,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_formats_and_round_trips() {
        let id = JobId::new(42, 7);
        assert_eq!(id.to_string(), "job-42-7");
        assert_eq!("job-42-7".parse::<JobId>().unwrap(), id);
        // Large ids survive the round trip too.
        let big = JobId::new(i64::MAX, 0);
        assert_eq!(big.to_string().parse::<JobId>().unwrap(), big);
    }

    #[test]
    fn job_id_rejects_malformed_input() {
        for bad in [
            "",
            "job-",
            "job-42",
            "job-42-",
            "job--7",
            "job-42-7-9",
            "job-a-7",
            "job-42-b",
            "job-+42-7",
            "job--42-7",
            "task-42-7",
            "JOB-42-7",
            "job-42- 7",
            "job-99999999999999999999-1", // overflows i64
        ] {
            let err = bad.parse::<JobId>().unwrap_err();
            assert_eq!(err, InvalidJobId(bad.to_string()), "input {bad:?}");
        }
    }
}
