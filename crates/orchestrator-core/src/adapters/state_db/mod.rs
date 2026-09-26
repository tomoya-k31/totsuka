//! SQLite-backed task state persistence (F-70, F-72, F-73) with embedded
//! migrations (§10.3).
//!
//! Design decisions (see #48):
//! - `tasks.state` is stored as TEXT for debuggability; `idx_tasks_state`
//!   keeps `status` fast enough at the target scale (§5.5).
//! - Task residue (labels/assignee/source status) lives in `source_payload`
//!   as JSON rather than per-column, absorbing source differences.
//! - `tasks.finished_at` is the retention anchor for worktree cleanup (#53).
//! - Migrations run on open inside a transaction; the DB file is backed up
//!   first when there are pending migrations, to `{path}.v{current}.bak` —
//!   the pre-migration schema version, so a two-version upgrade still leaves
//!   a way back to the intermediate one (§10.3, #275).
//! - `schema_migrations.applied_by` records the totsuka version that applied
//!   each row (#275). It is display/diagnostic only — schema version, not app
//!   version, is what compatibility is judged on.
//! - Only [`StateDb::open`] migrates; every command that does not hold
//!   `run.lock` uses [`StateDb::open_no_migrate`], so schema changes happen
//!   exclusively under the lock that `totsuka run` holds (#275). The split
//!   is by lock, not by read vs write — `task cancel` writes, and still must
//!   not migrate. A DB newer than this
//!   binary is refused at both entry points; forward compatibility is not
//!   offered, and the guard can only help between releases that have it.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
use crate::domain::task::{SourceTaskId, Task, TaskId};
use crate::ports::clock::Clock;

mod events;
mod hook_events;
mod migrations;
mod sessions;
mod task_messages;
mod tasks;
#[cfg(test)]
mod test_support;

pub use events::{EventExportFilter, EventRecord, ExportedEvent, ExportedTask, TaskNote};
pub use hook_events::{HookEventInsert, HookEventOutcome};
pub use sessions::SessionRecord;
pub use task_messages::{HandoffOutcome, TaskMessage, TaskMessageInsert, TaskMessageOutcome};
pub use tasks::{BadTimestamp, NewTask};

/// The `events.detail` key that marks a row as a **note** rather than a state
/// transition (#407), and whose value names the kind of note.
///
/// A separate key from the existing `kind` (`ingested` / `dispatch` /
/// `publish` / …), which every transition already uses and which therefore
/// cannot tell the two apart. `from_state == to_state` cannot either:
/// `Escalated → Escalated` is a legal transition.
pub const NOTE_KEY: &str = "note";

// `tasks.id` is stored as the bare rowid; the newtype is Rust-side only.
impl rusqlite::ToSql for TaskId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl rusqlite::types::FromSql for TaskId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        i64::column_result(value).map(TaskId)
    }
}

impl rusqlite::ToSql for SourceTaskId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl rusqlite::types::FromSql for SourceTaskId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(SourceTaskId)
    }
}

/// Errors from the state store.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Underlying SQLite error.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Filesystem error (backup, directory creation).
    #[error("state io error: {0}")]
    Io(#[from] std::io::Error),
    /// An illegal state transition was requested.
    #[error(transparent)]
    Transition(#[from] InvalidTransition),
    /// The task changed after the caller read it (#763). Nothing was written.
    #[error(transparent)]
    Conflict(#[from] TransitionConflict),
    /// JSON (de)serialization of `source_payload`/`detail` failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// A stored state string was not recognized.
    #[error(transparent)]
    UnknownState(#[from] UnknownState),
    /// No task with the given id.
    #[error("task not found: {0}")]
    NotFound(TaskId),
    /// No session with the given row id.
    #[error("session not found: {0}")]
    SessionNotFound(i64),
    /// [`StateDb::note_task`] was handed a `detail` with no [`NOTE_KEY`]
    /// string (#407).
    ///
    /// Refused rather than written: an unmarked row would still become the
    /// task's latest event and would therefore **hide** a real note from
    /// [`StateDb::open_notes`], leaving `totsuka status` silent about a task
    /// that is not moving — the exact failure the note exists to prevent.
    #[error("a task note must carry a `{NOTE_KEY}` string naming its kind, got: {0}")]
    NotANote(String),
    /// The DB's schema is newer than this binary understands — a downgrade
    /// (#275). Refusing beats running against a schema we cannot reason
    /// about: a purely additive version difference would otherwise not even
    /// raise an error, it would just quietly disagree.
    #[error(
        "state.db のスキーマバージョン v{found} は、この totsuka {app}（対応 v{supported}）\
         では扱えません{introduced_by} → totsuka を更新してください"
    )]
    SchemaTooNew {
        /// Schema version found in the ledger.
        found: i64,
        /// Highest version this binary knows how to apply.
        supported: i64,
        /// This binary's version, for the operator to compare against.
        app: String,
        /// Pre-rendered `。v{n} を導入したのは {version} です` clause, empty
        /// when the ledger has no `applied_by` for that version.
        introduced_by: String,
    },
    /// The DB's schema predates this binary and the caller opened it through
    /// an entry point that does not migrate (#275).
    #[error(
        "state.db のスキーマは v{found}、この totsuka は v{expected} を必要とします \
         → `totsuka run` を一度実行してマイグレーションを適用してください"
    )]
    SchemaOutdated {
        /// Schema version found in the ledger.
        found: i64,
        /// Version this binary needs.
        expected: i64,
    },
}

/// A transition refused because the task moved after the caller read it
/// (#763): another writer applied a transition in between, so the caller's
/// decision was made against a state that no longer exists.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "task {id} changed state while {event:?} was being applied \
     (read at version {expected}, now {actual_state} at version {actual})"
)]
pub struct TransitionConflict {
    /// The task.
    pub id: TaskId,
    /// The version the caller read.
    pub expected: i64,
    /// The version found at write time.
    pub actual: i64,
    /// The state found at write time.
    pub actual_state: TaskState,
    /// The event that was not applied.
    pub event: TaskEvent,
}

// An inherent method on the domain type, defined here so `TaskRef` keeps
// its fields private to the state DB (#763).
impl Task {
    /// The reference a state transition of this task must be applied
    /// through: it fails with [`StateError::Conflict`] if the task has moved
    /// since it was read.
    pub fn task_ref(&self) -> TaskRef {
        TaskRef {
            id: self.id,
            version: self.state_version,
        }
    }
}

/// A task as of one read: its id **and** the state version the caller's
/// decision was made against (#763).
///
/// The only way to ask [`StateDb::apply_event`] / [`StateDb::retry_task`]
/// for a transition. Bundling the two keeps an id from being passed as a
/// version, or one task's version with another's id; neither `Clone` nor
/// `Copy`, so applying a transition *consumes* it and hands back the updated
/// one — reusing the stale reference for the next write does not compile.
#[derive(Debug, PartialEq, Eq)]
pub struct TaskRef {
    id: TaskId,
    version: i64,
}

impl TaskRef {
    /// The task id.
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// The state version this reference was taken at.
    pub fn version(&self) -> i64 {
        self.version
    }
}

/// The SQLite state database.
pub struct StateDb {
    conn: Connection,
    clock: Arc<dyn Clock>,
}

/// Put the newest dispatched batch of a conversation back on the queue.
/// See [`StateDb::unprocess_last_batch`] for why the batch is found by id.
fn unprocess_last_batch_tx(conn: &Connection, task_id: TaskId) -> Result<usize, StateError> {
    Ok(conn.execute(
        "UPDATE task_messages SET processed_at = NULL \
         WHERE task_id = ?1 AND processed_at = ( \
             SELECT processed_at FROM task_messages \
             WHERE task_id = ?1 AND processed_at IS NOT NULL \
             ORDER BY id DESC LIMIT 1 \
         )",
        params![task_id],
    )?)
}

/// Apply a state-machine transition inside a caller-owned transaction.
///
/// Shared by [`StateDb::apply_event`] and
/// [`StateDb::append_task_message_reopening`] so the two can never disagree
/// about what a transition writes (state, `finished_at`, audit event).
///
/// `expected` is the state version the caller read (#763); `None` only for a
/// caller that read the state inside this same transaction. The version is
/// compared **before** the transition is judged, so a task another writer
/// moved is reported as [`StateError::Conflict`] rather than as whatever the
/// stale event happens to mean in the new state.
fn apply_event_tx(
    conn: &Connection,
    now: &str,
    id: TaskId,
    expected: Option<i64>,
    event: TaskEvent,
    detail: Option<&str>,
) -> Result<(TaskState, i64), StateError> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT state, state_version FROM tasks WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (from, version) = row.ok_or(StateError::NotFound(id))?;
    let from: TaskState = from.parse()?;
    if let Some(expected) = expected.filter(|&v| v != version) {
        return Err(TransitionConflict {
            id,
            expected,
            actual: version,
            actual_state: from,
            event,
        }
        .into());
    }
    let to = transition(from, event)?;
    let finished_at = to.is_terminal().then(|| now.to_string());
    conn.execute(
        "UPDATE tasks SET state = ?1, updated_at = ?2, finished_at = ?3, \
         state_version = state_version + 1 WHERE id = ?4",
        params![to.as_str(), now, finished_at, id],
    )?;
    if event == TaskEvent::Dispatch {
        // A dispatch starts a NEW execution, so the D-03 silence anchor from
        // the previous one must not carry over (#382). It did: a task that got
        // a hook signal, failed, and was `task retry`d minutes later was swept
        // as "silent for longer than `timeout_secs`" and escalated **3ms after
        // being dispatched** — before its fresh agent could emit anything.
        //
        // Cleared rather than set to `now`, which keeps `last_signal_at`
        // meaning exactly what its name says (the last hook signal) and leaves
        // a re-dispatched task in the same position as a first-dispatched one:
        // `sweep_signal_timeouts` skips a task with no anchor, so D-03 starts
        // protecting it once it has proven itself alive. Anchoring at dispatch
        // instead would extend D-03 to "never came alive", which it has never
        // covered and which the `pane.exited` deadman owns.
        conn.execute(
            "UPDATE tasks SET last_signal_at = NULL WHERE id = ?1",
            params![id],
        )?;
    }
    conn.execute(
        "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, from.as_str(), to.as_str(), now, detail],
    )?;
    Ok((to, version + 1))
}

/// Wrap a domain error as a rusqlite column-conversion failure.
fn conversion_error(e: Box<dyn std::error::Error + Send + Sync>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e)
}
