//! Task identity.

use std::fmt;

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
