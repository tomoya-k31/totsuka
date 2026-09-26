//! A task and its identity.

use std::fmt;

use time::OffsetDateTime;

use super::state::TaskState;
use super::workflow::WorkflowMode;

/// A task's row id (`tasks.id`) — the id totsuka itself assigns.
///
/// Not the source's own id for the task (the Issue number, the Notion page
/// id): that one is what a pane label and the plugin protocol carry. Both
/// used to be bare integers/strings, so passing one where the other belonged
/// compiled silently (#210, #765).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct TaskId(pub i64);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The source's own id for a task (`tasks.source_task_id`): the Issue number,
/// the Notion page id, the Slack thread key.
///
/// What a pane label and the plugin protocol's `Task.id` carry — never the
/// [`TaskId`] row id (#210, #765).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct SourceTaskId(pub String);

/// A task as the state DB holds it (F-71/72).
///
/// Timestamps are `OffsetDateTime`: turning them into text is the state DB's
/// business on the way in and out, and the CLI's when it prints them — a
/// string compare cannot creep back in and sort `…00.5Z` after `…00.53Z`
/// (#478, #765).
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// Row id.
    pub id: TaskId,
    /// Source plugin instance name.
    pub source: String,
    /// Source's own task id.
    pub source_task_id: SourceTaskId,
    /// Matched workflow name.
    pub workflow: String,
    /// Execution mode.
    pub mode: WorkflowMode,
    /// Selected repository name.
    pub repo: Option<String>,
    /// worktree path once created.
    pub worktree_path: Option<String>,
    /// Branch name once created.
    pub branch: Option<String>,
    /// The commit the worktree was branched from, once created (v8).
    pub base_commit: Option<String>,
    /// Current state.
    pub state: TaskState,
    /// Priority.
    pub priority: i64,
    /// Title.
    pub title: String,
    /// URL.
    pub url: Option<String>,
    /// Residual source fields.
    pub source_payload: Option<serde_json::Value>,
    /// Terminal-state timestamp (retention anchor).
    pub finished_at: Option<OffsetDateTime>,
    /// Ingest timestamp.
    pub created_at: OffsetDateTime,
    /// Last-update timestamp.
    pub updated_at: OffsetDateTime,
    /// Timestamp of the last hook signal (R-10 timeout anchor).
    pub last_signal_at: Option<OffsetDateTime>,
    /// Bumped by every state transition (#763) — the version a
    /// `TaskRef` carries.
    pub state_version: i64,
}
