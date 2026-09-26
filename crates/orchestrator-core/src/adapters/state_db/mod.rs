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

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::domain::EventDetail;
use crate::domain::state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
use crate::domain::task::{SourceTaskId, Task, TaskId};
use crate::domain::workflow::WorkflowMode;
use crate::ports::clock::Clock;

mod events;
mod migrations;
mod tasks;
#[cfg(test)]
mod test_support;

pub use events::{EventExportFilter, EventRecord, ExportedEvent, ExportedTask, TaskNote};
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

/// Columns of `sessions`, read by name in [`row_to_session`].
const SESSION_COLUMNS: &str = "id, task_id, plugin, session_id, created_at, tool_session_id";

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

/// A persisted agent session (F-37): the `session_id` returned by
/// `task/dispatch`, linked to its task and owning plugin.
///
/// A task may accumulate several rows — a retry starts a fresh session — so the
/// newest row is the re-attach target ([`StateDb::latest_session`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Row id.
    pub id: i64,
    /// Owning task id.
    pub task_id: TaskId,
    /// Plugin instance name that owns the session (e.g. `herdr`).
    pub plugin: String,
    /// The agent's opaque conversation/session id.
    pub session_id: String,
    /// Creation timestamp (ISO 8601 UTC).
    pub created_at: String,
    /// The tool's own native `session_id` for this dispatch (Claude Code
    /// today), once observed via a hook (E-09 correlation / resume). `None`
    /// until a SessionStart-bearing signal records it.
    pub tool_session_id: Option<String>,
}

/// A hook event to persist idempotently (#131 D-05 / N-01).
///
/// The idempotency key is `(job_id, tool_session_id, prompt_id, event, status)`;
/// the optional components are empty strings (not `None`) so SQLite's UNIQUE
/// constraint actually dedups repeated deliveries (multiple hook fires, spool
/// re-sends, curl retries). `status` joined the key in v3 so a block →
/// re-completion pair of `Stop`s is not collapsed into one (#154).
#[derive(Debug, Clone)]
pub struct HookEventInsert {
    /// The dispatch this event belongs to (`TOTSUKA_JOB_ID`, E-09).
    pub job_id: String,
    /// Owning task id (resolved from `job_id`, never guessed from a session).
    pub task_id: TaskId,
    /// The tool-native `session_id` (empty if the hook input lacked one).
    pub tool_session_id: String,
    /// The hook input's `prompt_id` (empty if absent).
    pub prompt_id: String,
    /// Event kind: `stop` / `notification` / `session_start` / `session_end` /
    /// `heartbeat`.
    pub event: String,
    /// For `stop`: the self-reported outcome
    /// (`COMPLETED`/`NEEDS_INPUT`/`FAILED`/`UNKNOWN`); `None` otherwise.
    pub status: Option<String>,
    /// The full received JSON, verbatim (audit, N-01).
    pub payload: String,
}

/// Outcome of [`StateDb::record_hook_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEventOutcome {
    /// The event was inserted (first time seen).
    New,
    /// A row with the same idempotency key already existed; nothing changed and
    /// the caller drops it silently.
    Duplicate,
}

/// One message to append to a conversation's ledger (#242).
#[derive(Debug, Clone)]
pub struct TaskMessageInsert {
    /// The conversation this delivery belongs to.
    pub task_id: TaskId,
    /// Identity of *this* delivery within the conversation — `Task.message_key`
    /// (the source falls back to `Task.id` when it has only one message).
    pub message_key: String,
    /// Display-only author, denormalized out of `payload`.
    pub author: Option<String>,
    /// The message text the agent will be prompted with.
    pub body: String,
    /// Permalink to the message in the source system.
    pub url: Option<String>,
    /// The whole normalized `Task` as JSON, verbatim (audit, N-01).
    pub payload: String,
}

/// A stored conversation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskMessage {
    /// Row id; also the arrival order within a conversation.
    pub id: i64,
    /// Owning conversation.
    pub task_id: TaskId,
    /// Identity of this delivery.
    pub message_key: String,
    /// Display-only author.
    pub author: Option<String>,
    /// The message text.
    pub body: String,
    /// Permalink.
    pub url: Option<String>,
    /// The whole normalized `Task` as JSON.
    pub payload: String,
    /// When the message was appended.
    pub received_at: String,
    /// When it was dispatched to the agent; `None` while it is still queued.
    /// Every message dispatched together carries the same value.
    pub processed_at: Option<String>,
}

/// Outcome of [`StateDb::append_task_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMessageOutcome {
    /// The message was appended (first time seen).
    New,
    /// The conversation already had this `message_key`; nothing changed.
    Duplicate,
}

/// Outcome of [`StateDb::append_task_message_handing_off`] (#565).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// The conversation already had this `message_key`; nothing was written.
    Duplicate,
    /// The conversation is still in flight, so it was **not** handed over and
    /// **nothing was written** — not even the message. Writing it would strand
    /// the delivery: the ledger would dedup every re-delivery from then on
    /// while the row kept its old workflow, and no later poll could repair it.
    /// Left unwritten, the source re-delivers until the run finishes and the
    /// handoff can happen for real.
    InFlight,
    /// The message was appended, the conversation reopened, and its workflow /
    /// mode / payload now name the delivering workflow. The row is `Queued`.
    HandedOff,
}

/// Columns of `task_messages`, read by name in [`row_to_task_message`].
const TASK_MESSAGE_COLUMNS: &str = "id, task_id, message_key, author, body, url, \
     payload, received_at, processed_at";

/// The SQLite state database.
pub struct StateDb {
    conn: Connection,
    clock: Arc<dyn Clock>,
}

impl StateDb {
    /// Persist the session id returned by `task/dispatch` (F-37), linking it to
    /// its task and the owning plugin.
    ///
    /// Appends a new row rather than replacing, so a retried task keeps its
    /// session history; [`latest_session`](Self::latest_session) exposes the
    /// newest one as the re-attach target. Returns the new row id.
    pub fn record_session(
        &self,
        task_id: TaskId,
        plugin: &str,
        session_id: &str,
    ) -> Result<i64, StateError> {
        // Report an unknown task as NotFound rather than surfacing the raw
        // foreign-key violation, matching the other setters' contract.
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![task_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StateError::NotFound(task_id));
        }
        self.conn.execute(
            "INSERT INTO sessions (task_id, plugin, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, plugin, session_id, self.clock.now_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Reserve a session row *before* `task/dispatch`, so its id can seed the
    /// hook correlation key `job_id = job-{task_id}-{session_row}` (#131 E-09).
    ///
    /// The job id must be injected into the agent process **at launch** (it is
    /// echoed back by every hook), yet the agent-native session id is only known
    /// once `task/dispatch` returns — so the row is created here with an empty
    /// native id and filled in afterwards by
    /// [`set_session_native_id`](Self::set_session_native_id). Returns the new
    /// row id (the `session_row` component of the job id).
    pub fn reserve_session(&self, task_id: TaskId, plugin: &str) -> Result<i64, StateError> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![task_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StateError::NotFound(task_id));
        }
        self.conn.execute(
            "INSERT INTO sessions (task_id, plugin, session_id, created_at)
             VALUES (?1, ?2, '', ?3)",
            params![task_id, plugin, self.clock.now_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fill in the agent-native session id on a row created by
    /// [`reserve_session`](Self::reserve_session), once `task/dispatch` has
    /// returned it (the hook-dispatch counterpart of
    /// [`record_session`](Self::record_session)).
    pub fn set_session_native_id(
        &self,
        session_row_id: i64,
        session_id: &str,
    ) -> Result<(), StateError> {
        let n = self.conn.execute(
            "UPDATE sessions SET session_id = ?1 WHERE id = ?2",
            params![session_id, session_row_id],
        )?;
        if n == 0 {
            return Err(StateError::SessionNotFound(session_row_id));
        }
        Ok(())
    }

    /// Delete a session row by id. Used to roll back a
    /// [`reserve_session`](Self::reserve_session) reservation when the
    /// subsequent `task/dispatch` fails, so a failed dispatch leaves no
    /// empty-id row for retry / recovery to trip over. A missing row is not an
    /// error (the rollback is best-effort).
    pub fn delete_session(&self, session_row_id: i64) -> Result<(), StateError> {
        self.conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![session_row_id],
        )?;
        Ok(())
    }

    /// The most recent session for a task — the re-attach target (F-37) — or
    /// `None` if the task was never dispatched.
    ///
    /// Ordered by `id`, **not** `created_at` (#478). `created_at` is an RFC3339
    /// *string* whose subsecond part is variable-width, so lexicographic order
    /// is not chronological order: when the earlier timestamp is a prefix of
    /// the later one, the next character compared is a digit against `Z`, and
    /// the older row sorts as the newer (`…10.28357Z` > `…10.283572Z`).
    /// Measured over 2,000 samples, that inverted 278 of the 1,999 consecutive
    /// pairs — roughly one in seven. `id` is the
    /// rowid, assigned in insertion order, and a reused rowid is only ever
    /// handed out above every surviving row — so it orders these rows exactly
    /// as `created_at` was meant to.
    pub fn latest_session(&self, task_id: TaskId) -> Result<Option<SessionRecord>, StateError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 \
             ORDER BY id DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![task_id], row_to_session)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// All sessions for a task, newest first (session history for `status`).
    ///
    /// Ordered by `id` for the reason spelled out on
    /// [`latest_session`](Self::latest_session) (#478).
    pub fn list_sessions(&self, task_id: TaskId) -> Result<Vec<SessionRecord>, StateError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 \
             ORDER BY id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![task_id], row_to_session)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Persist a hook event idempotently (#131 D-05 / N-01).
    ///
    /// `INSERT ... ON CONFLICT DO NOTHING` on the idempotency key
    /// `(job_id, tool_session_id, prompt_id, event, status)`. A repeat delivery
    /// with the *same status* (multiple hook fires, spool re-send, curl retry)
    /// leaves the log unchanged and returns [`HookEventOutcome::Duplicate`], which
    /// the caller drops silently. `status` is part of the key so a `block`-driven
    /// re-completion within the same turn (`UNKNOWN` → `COMPLETED`, same
    /// `prompt_id`) is recorded rather than dropped as a re-delivery. A `None`
    /// status is stored as `''` to match the `NOT NULL DEFAULT ''` column (SQLite
    /// treats NULLs as distinct under UNIQUE, which would defeat the dedup).
    pub fn record_hook_event(&self, evt: &HookEventInsert) -> Result<HookEventOutcome, StateError> {
        let changed = self.conn.execute(
            "INSERT INTO hook_events
                (job_id, task_id, tool_session_id, prompt_id, event, status,
                 payload, received_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT (job_id, tool_session_id, prompt_id, event, status)
                DO NOTHING",
            params![
                evt.job_id,
                evt.task_id,
                evt.tool_session_id,
                evt.prompt_id,
                evt.event,
                evt.status.as_deref().unwrap_or(""),
                evt.payload,
                self.clock.now_rfc3339(),
            ],
        )?;
        Ok(if changed > 0 {
            HookEventOutcome::New
        } else {
            HookEventOutcome::Duplicate
        })
    }

    /// Append a message to a conversation, idempotently (#242).
    ///
    /// `INSERT ... ON CONFLICT DO NOTHING` on `(task_id, message_key)`, the
    /// same shape as [`record_hook_event`](Self::record_hook_event) and for the
    /// same reason: sources deliver at-least-once (a Socket Mode reconnect, a
    /// restart mid-ack), and a re-delivery must not queue the work twice.
    pub fn append_task_message(
        &self,
        msg: &TaskMessageInsert,
    ) -> Result<TaskMessageOutcome, StateError> {
        let changed = insert_task_message_tx(&self.conn, msg, &self.clock.now_rfc3339())?;
        Ok(if changed > 0 {
            TaskMessageOutcome::New
        } else {
            TaskMessageOutcome::Duplicate
        })
    }

    /// Append a message and, **in the same transaction**, requeue the
    /// conversation if it had already finished (#242).
    ///
    /// The two must be atomic. Done separately, a crash in between leaves the
    /// task terminal with an unprocessed message in its ledger — and because
    /// the message *is* recorded, the source's re-delivery dedups to
    /// [`TaskMessageOutcome::Duplicate`] and never reopens it, so that message
    /// is stranded forever with nothing to notice it. (`upsert_task` before
    /// this call needs no such coupling: a crash there leaves a task with an
    /// empty ledger, and the re-delivery simply appends.)
    ///
    /// Returns the append outcome and the state the conversation ended up in
    /// when it was reopened (`None` when nothing was reopened — either the
    /// message was a duplicate, or the conversation was still in flight).
    pub fn append_task_message_reopening(
        &self,
        msg: &TaskMessageInsert,
        detail: Option<EventDetail>,
    ) -> Result<(TaskMessageOutcome, Option<TaskState>), StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(EventDetail::to_json).transpose()?;
        let tx = self.conn.unchecked_transaction()?;
        let changed = insert_task_message_tx(&tx, msg, &now)?;
        if changed == 0 {
            tx.commit()?;
            return Ok((TaskMessageOutcome::Duplicate, None));
        }
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![msg.task_id],
                |r| r.get(0),
            )
            .optional()?;
        let state: TaskState = state.ok_or(StateError::NotFound(msg.task_id))?.parse()?;
        let reopened = if state.is_terminal() {
            // Read and written in this one transaction: nothing can move in
            // between, so there is no version to hold it to.
            Some(
                apply_event_tx(
                    &tx,
                    &now,
                    msg.task_id,
                    None,
                    TaskEvent::Reopen,
                    detail.as_deref(),
                )?
                .0,
            )
        } else {
            None
        };
        tx.commit()?;
        Ok((TaskMessageOutcome::New, reopened))
    }

    /// Append a message that arrived under a **different workflow** and, in
    /// the same transaction, hand the conversation over to it (#565).
    ///
    /// A conversation is one row keyed by `(source, source_task_id)`, and its
    /// `workflow` was fixed when the row was created (`ON CONFLICT DO
    /// NOTHING` never updates it). A column pipeline — design finishes, the
    /// card lands in the implement column — therefore delivered under a
    /// workflow the row did not belong to, and the delivery was dropped. This
    /// moves the row to the delivering workflow instead, so the same
    /// conversation (and its worktree, and its agent session) continues into
    /// the next stage.
    ///
    /// Three columns move together and all three are load-bearing:
    ///
    /// - `workflow` — everything downstream resolves settings by this name on
    ///   each use (dispatch target, status write-backs, verification, tool),
    ///   so it switches the whole stage.
    /// - `mode` — dispatch reads the **column**, not the workflow, to pick
    ///   [`ExecutionMode`](plugin_protocol::methods::ExecutionMode); leaving it
    ///   would run the implement stage under plan's restrictions. It also
    ///   routes worktree cleanup and the plan side-effect check.
    /// - `source_payload` — `task_from_record` rebuilds the dispatched task
    ///   from it, so leaving it would hand the new stage the **previous**
    ///   stage's instructions.
    ///
    /// **Atomic, and the atomicity is the point.** A crash between the append
    /// and the update leaves the row on its old workflow with the new message
    /// already in the ledger — which every re-delivery then dedups against,
    /// permanently. That is the same stranding
    /// [`append_task_message_reopening`](Self::append_task_message_reopening)
    /// exists to avoid, reached from the other side.
    ///
    /// Only a **terminal** conversation is handed over; see
    /// [`HandoffOutcome::InFlight`] for why the in-flight case writes nothing.
    pub fn append_task_message_handing_off(
        &self,
        msg: &TaskMessageInsert,
        new_workflow: &str,
        new_mode: WorkflowMode,
        new_source_payload: Option<&serde_json::Value>,
        detail: Option<EventDetail>,
    ) -> Result<HandoffOutcome, StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(EventDetail::to_json).transpose()?;
        let payload = new_source_payload.map(serde_json::to_string).transpose()?;
        let tx = self.conn.unchecked_transaction()?;

        // State first: an in-flight conversation must leave the ledger
        // untouched, so this cannot run after the insert.
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![msg.task_id],
                |r| r.get(0),
            )
            .optional()?;
        let state: TaskState = state.ok_or(StateError::NotFound(msg.task_id))?.parse()?;
        if !state.is_terminal() {
            tx.commit()?;
            return Ok(HandoffOutcome::InFlight);
        }

        if insert_task_message_tx(&tx, msg, &now)? == 0 {
            tx.commit()?;
            return Ok(HandoffOutcome::Duplicate);
        }
        apply_event_tx(
            &tx,
            &now,
            msg.task_id,
            None,
            TaskEvent::Reopen,
            detail.as_deref(),
        )?;
        tx.execute(
            "UPDATE tasks SET workflow = ?2, mode = ?3, source_payload = ?4, updated_at = ?5 \
             WHERE id = ?1",
            params![msg.task_id, new_workflow, new_mode.as_str(), payload, now],
        )?;
        tx.commit()?;
        Ok(HandoffOutcome::HandedOff)
    }

    /// Ids of tasks in `state` that still have messages nobody has sent
    /// (#242).
    ///
    /// One query rather than "list the tasks, then ask each about its ledger":
    /// the run loop calls this on every 200 ms tick, and the number of
    /// finished conversations only grows.
    pub fn conversations_with_unsent_messages(
        &self,
        state: TaskState,
    ) -> Result<Vec<TaskRef>, StateError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT t.id, t.state_version FROM tasks t \
             JOIN task_messages m ON m.task_id = t.id \
             WHERE t.state = ?1 AND m.processed_at IS NULL \
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![state.as_str()], |r| {
            Ok(TaskRef {
                id: r.get(0)?,
                version: r.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// The conversation's undispatched messages, oldest first — its queue.
    ///
    /// Ordered by `id` rather than `received_at`: arrival order is what the
    /// agent should read them in, and `id` gives it without depending on
    /// timestamp resolution.
    pub fn pending_task_messages(&self, task_id: TaskId) -> Result<Vec<TaskMessage>, StateError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_MESSAGE_COLUMNS} FROM task_messages \
             WHERE task_id = ?1 AND processed_at IS NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![task_id], row_to_task_message)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Every message of a conversation, oldest first (display).
    pub fn list_task_messages(&self, task_id: TaskId) -> Result<Vec<TaskMessage>, StateError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_MESSAGE_COLUMNS} FROM task_messages \
             WHERE task_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![task_id], row_to_task_message)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Mark every pending message of a conversation as dispatched, stamping
    /// them all with **one** timestamp, and return it.
    ///
    /// The shared stamp is what makes a batch identifiable afterwards without
    /// a batch-id column — see
    /// [`unprocess_last_batch`](Self::unprocess_last_batch).
    pub fn mark_messages_processed(&self, task_id: TaskId) -> Result<String, StateError> {
        let at = self.clock.now_rfc3339();
        self.conn.execute(
            "UPDATE task_messages SET processed_at = ?1 \
             WHERE task_id = ?2 AND processed_at IS NULL",
            params![at, task_id],
        )?;
        Ok(at)
    }

    /// Put the most recently dispatched batch back on the queue (`task retry`).
    ///
    /// The batch is found by the **highest-id processed row** and then matched
    /// by its exact `processed_at` string. Picking it by id rather than by
    /// `MAX(processed_at)` is deliberate: RFC 3339 with optional fractional
    /// seconds does not sort lexicographically (`…:00.5Z` < `…:00Z`), while
    /// ids are integers and messages are appended in arrival order, so the
    /// newest processed row always belongs to the newest batch.
    ///
    /// Returns how many messages were requeued (0 when nothing was ever
    /// dispatched). Two batches stamped with the *same* timestamp would be
    /// requeued together; that needs a clock that did not advance between
    /// dispatches, which only a frozen test clock does.
    pub fn unprocess_last_batch(&self, task_id: TaskId) -> Result<usize, StateError> {
        unprocess_last_batch_tx(&self.conn, task_id)
    }

    /// Number of consecutive `UNKNOWN` stops at the tail of a task's stop
    /// history — the D-02 escalation counter (recomputed from the log; the
    /// hook's self-reported `block_count` is never trusted).
    ///
    /// Scans stop events id-descending and counts the leading `UNKNOWN` run
    /// until the first non-`UNKNOWN` stop; a `COMPLETED`/`NEEDS_INPUT`/`FAILED`
    /// stop resets the streak. Backed by `idx_hook_events_task`; the early
    /// break keeps it ~O(streak) (≈ O(3) at the escalation threshold).
    pub fn unknown_stop_streak(&self, task_id: TaskId) -> Result<u32, StateError> {
        let mut stmt = self.conn.prepare(
            "SELECT status FROM hook_events \
             WHERE task_id = ?1 AND event = 'stop' ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![task_id], |r| r.get::<_, Option<String>>(0))?;
        let mut streak = 0u32;
        for status in rows {
            if status?.as_deref() == Some("UNKNOWN") {
                streak += 1;
            } else {
                break;
            }
        }
        Ok(streak)
    }

    /// Record the tool's own native `session_id` on a dispatch's session row
    /// (E-09 correlation / resume).
    pub fn set_tool_session_id(
        &self,
        session_row_id: i64,
        tool_session_id: &str,
    ) -> Result<(), StateError> {
        let n = self.conn.execute(
            "UPDATE sessions SET tool_session_id = ?1 WHERE id = ?2",
            params![tool_session_id, session_row_id],
        )?;
        if n == 0 {
            return Err(StateError::SessionNotFound(session_row_id));
        }
        Ok(())
    }

    /// Find the most recent session bearing a given tool-native `session_id`.
    pub fn find_session_by_tool_session_id(
        &self,
        tool_session_id: &str,
    ) -> Result<Option<SessionRecord>, StateError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE tool_session_id = ?1 ORDER BY id DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![tool_session_id], row_to_session)?;
        rows.next().transpose().map_err(StateError::from)
    }
}

/// Map a `sessions` row (in [`SESSION_COLUMNS`] order) to a [`SessionRecord`].
fn row_to_session(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        plugin: row.get("plugin")?,
        session_id: row.get("session_id")?,
        created_at: row.get("created_at")?,
        tool_session_id: row.get("tool_session_id")?,
    })
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

/// Insert one ledger row, ignoring a `(task_id, message_key)` collision.
/// Returns the number of rows written (0 = the message was already there).
fn insert_task_message_tx(
    conn: &Connection,
    msg: &TaskMessageInsert,
    now: &str,
) -> Result<usize, StateError> {
    Ok(conn.execute(
        "INSERT INTO task_messages
            (task_id, message_key, author, body, url, payload, received_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT (task_id, message_key) DO NOTHING",
        params![
            msg.task_id,
            msg.message_key,
            msg.author,
            msg.body,
            msg.url,
            msg.payload,
            now,
        ],
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

/// Map a `task_messages` row (selected via [`TASK_MESSAGE_COLUMNS`]).
fn row_to_task_message(row: &Row<'_>) -> rusqlite::Result<TaskMessage> {
    Ok(TaskMessage {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        message_key: row.get("message_key")?,
        author: row.get("author")?,
        body: row.get("body")?,
        url: row.get("url")?,
        payload: row.get("payload")?,
        received_at: row.get("received_at")?,
        processed_at: row.get("processed_at")?,
    })
}

/// Wrap a domain error as a rusqlite column-conversion failure.
fn conversion_error(e: Box<dyn std::error::Error + Send + Sync>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e)
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    use std::fs;

    use crate::domain::event_detail::Reopen;
    use crate::ports::clock::format_rfc3339;

    /// At-least-once delivery must not queue the same message twice (#242).
    #[test]
    fn appending_a_message_twice_is_a_no_op() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        assert_eq!(
            db.append_task_message(&message(id, "m1", "first")).unwrap(),
            TaskMessageOutcome::New
        );
        assert_eq!(
            db.append_task_message(&message(id, "m1", "re-delivered"))
                .unwrap(),
            TaskMessageOutcome::Duplicate
        );

        let all = db.list_task_messages(id).unwrap();
        assert_eq!(keys(&all), ["m1"], "the duplicate must not add a row");
        assert_eq!(all[0].body, "first", "and must not overwrite the original");
    }

    /// The pending set is the queue: undispatched only, in arrival order.
    #[test]
    fn pending_messages_are_unprocessed_ones_in_arrival_order() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        for key in ["m1", "m2", "m3"] {
            db.append_task_message(&message(id, key, key)).unwrap();
        }
        assert_eq!(
            keys(&db.pending_task_messages(id).unwrap()),
            ["m1", "m2", "m3"]
        );

        db.mark_messages_processed(id).unwrap();
        assert!(db.pending_task_messages(id).unwrap().is_empty());
        // A message arriving after the dispatch is pending on its own.
        db.append_task_message(&message(id, "m4", "m4")).unwrap();
        assert_eq!(keys(&db.pending_task_messages(id).unwrap()), ["m4"]);
        // ...while the full history still shows everything.
        assert_eq!(
            keys(&db.list_task_messages(id).unwrap()),
            ["m1", "m2", "m3", "m4"]
        );
    }

    /// A dispatched batch shares one `processed_at`, and `task retry` puts
    /// exactly that batch — not the ones before it — back on the queue (D7).
    #[test]
    fn unprocess_last_batch_requeues_only_the_newest_batch() {
        let clock = manual_clock();
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // Batch 1.
        db.append_task_message(&message(id, "m1", "m1")).unwrap();
        db.append_task_message(&message(id, "m2", "m2")).unwrap();
        let first = db.mark_messages_processed(id).unwrap();

        // Batch 2, at a later instant.
        clock.advance(time::Duration::seconds(60));
        db.append_task_message(&message(id, "m3", "m3")).unwrap();
        let second = db.mark_messages_processed(id).unwrap();
        assert_ne!(first, second);

        let stamps: Vec<Option<String>> = db
            .list_task_messages(id)
            .unwrap()
            .into_iter()
            .map(|m| m.processed_at)
            .collect();
        assert_eq!(
            stamps,
            [
                Some(first.clone()),
                Some(first.clone()),
                Some(second.clone())
            ],
            "a batch is exactly the rows sharing one stamp"
        );

        assert_eq!(db.unprocess_last_batch(id).unwrap(), 1);
        assert_eq!(
            keys(&db.pending_task_messages(id).unwrap()),
            ["m3"],
            "only the newest batch comes back"
        );
        // Doing it again reaches the batch before it — one step per retry.
        assert_eq!(db.unprocess_last_batch(id).unwrap(), 2);
        assert_eq!(
            keys(&db.pending_task_messages(id).unwrap()),
            ["m1", "m2", "m3"]
        );
        // Nothing left to requeue.
        assert_eq!(db.unprocess_last_batch(id).unwrap(), 0);
    }

    /// Ledgers are per-conversation: one task's queue never leaks into
    /// another's, and the UNIQUE key is scoped to the task so two
    /// conversations may legitimately carry the same `message_key`.
    #[test]
    fn message_ledgers_are_isolated_per_task() {
        let db = StateDb::open_in_memory().unwrap();
        let a = db.upsert_task(&sample_task()).unwrap();
        let b = db
            .upsert_task(&NewTask {
                source_task_id: SourceTaskId("43".to_string()),
                ..sample_task()
            })
            .unwrap();
        assert_ne!(a, b);

        db.append_task_message(&message(a, "shared", "for a"))
            .unwrap();
        assert_eq!(
            db.append_task_message(&message(b, "shared", "for b"))
                .unwrap(),
            TaskMessageOutcome::New,
            "the same key in another conversation is a different message"
        );

        db.mark_messages_processed(a).unwrap();
        assert!(db.pending_task_messages(a).unwrap().is_empty());
        assert_eq!(
            keys(&db.pending_task_messages(b).unwrap()),
            ["shared"],
            "the other conversation's queue is untouched"
        );
    }

    #[test]
    fn sessions_append_and_latest_wins() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // No dispatch yet -> no session to re-attach.
        assert!(db.latest_session(id).unwrap().is_none());

        let s1 = db.record_session(id, "herdr", "sess-1").unwrap();
        let s2 = db.record_session(id, "herdr", "sess-2").unwrap();
        assert_ne!(s1, s2, "each dispatch appends a distinct session row");

        // Latest (highest id) is the re-attach target.
        let latest = db.latest_session(id).unwrap().unwrap();
        assert_eq!(latest.session_id, "sess-2");
        assert_eq!(latest.plugin, "herdr");
        assert_eq!(latest.task_id, id);

        // History keeps both, newest first.
        let all = db.list_sessions(id).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].session_id, "sess-2");
        assert_eq!(all[1].session_id, "sess-1");
    }

    /// The re-attach target must not depend on how `created_at` happens to
    /// *format* (#478).
    ///
    /// `time`'s RFC3339 writes only as many subsecond digits as it needs, so
    /// consecutive timestamps differ in width — and when the earlier one is a
    /// prefix of the later one, string comparison puts the **older** row first
    /// (`.5Z` vs `.53Z`: `Z` is 0x5A, `3` is 0x33). This test pins exactly that
    /// pair through the injected clock, so it is deterministic rather than a
    /// 14%-of-the-time flake, and it fails against `ORDER BY created_at DESC`
    /// even with an `id DESC` tiebreak — the tiebreak never runs, because the
    /// two timestamps do compare unequal.
    #[test]
    fn latest_session_ignores_how_the_timestamp_string_sorts() {
        let clock = manual_clock();
        // …T00:00:00.5Z — one subsecond digit.
        clock.advance(time::Duration::milliseconds(500));
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let first = db.record_session(id, "herdr", "sess-1").unwrap();

        // …T00:00:00.53Z — later in time, *smaller* as a string.
        clock.advance(time::Duration::milliseconds(30));
        let second = db.record_session(id, "herdr", "sess-2").unwrap();
        assert!(second > first, "the rowid still increases");

        let rows = db.list_sessions(id).unwrap();
        let stamps: Vec<&str> = rows.iter().map(|r| r.created_at.as_str()).collect();
        assert!(
            stamps.contains(&"2026-01-01T00:00:00.5Z")
                && stamps.contains(&"2026-01-01T00:00:00.53Z"),
            "the fixture must actually produce the prefix pair, got {stamps:?}"
        );

        assert_eq!(
            db.latest_session(id).unwrap().unwrap().session_id,
            "sess-2",
            "the newest dispatch is the re-attach target, whatever the string sort says"
        );
        assert_eq!(
            rows[0].session_id, "sess-2",
            "history is newest-first by the same order"
        );
    }

    #[test]
    fn record_session_rejects_unknown_task() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(matches!(
            db.record_session(TaskId(999), "herdr", "sess-x")
                .unwrap_err(),
            StateError::NotFound(TaskId(999))
        ));
    }

    #[test]
    fn sessions_survive_reopen_from_disk() {
        // kill-and-restart: the re-attach target must survive a process exit.
        let dir =
            std::env::temp_dir().join(format!("totsuka-{}-session_reopen", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state.db");

        let id = {
            let db = StateDb::open(&path).unwrap();
            let id = db.upsert_task(&sample_task()).unwrap();
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
                .unwrap();
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Start, None)
                .unwrap();
            db.record_session(id, "herdr", "sess-live").unwrap();
            id
        }; // db dropped, simulating process exit

        let db = StateDb::open(&path).unwrap();
        let latest = db.latest_session(id).unwrap().unwrap();
        assert_eq!(latest.session_id, "sess-live");
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Appending to a finished conversation requeues it, and the two writes
    /// are one transaction so a message can never be recorded without the
    /// reopen that makes it reachable.
    #[test]
    fn append_reopens_a_finished_conversation_atomically() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            db.apply_event(db.task_ref(id).unwrap(), event, None)
                .unwrap();
        }
        let before = db.get_task(id).unwrap().unwrap();
        assert!(before.finished_at.is_some());

        let (outcome, reopened) = db
            .append_task_message_reopening(
                &message(id, "m2", "a follow-up"),
                Some(EventDetail::Reopen(Reopen::Message {
                    message_key: "m2".to_string(),
                })),
            )
            .unwrap();
        assert_eq!(outcome, TaskMessageOutcome::New);
        assert_eq!(reopened, Some(TaskState::Queued));

        let after = db.get_task(id).unwrap().unwrap();
        assert_eq!(after.state, TaskState::Queued);
        assert!(
            after.finished_at.is_none(),
            "leaving a terminal state clears the retention anchor"
        );
        assert_eq!(keys(&db.pending_task_messages(id).unwrap()), ["m2"]);

        // A conversation still in flight is appended to, never transitioned.
        let (outcome, reopened) = db
            .append_task_message_reopening(&message(id, "m3", "another"), None)
            .unwrap();
        assert_eq!((outcome, reopened), (TaskMessageOutcome::New, None));
        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::Queued,
            "a non-terminal conversation is left alone"
        );
    }

    #[test]
    fn record_hook_event_dedups_on_conflict() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // Empty session/prompt ids still dedup — the UNIQUE columns default to
        // '' so SQLite does not treat repeated deliveries as distinct.
        let evt = hook_event(id, "job-1-1", "stop", Some("COMPLETED"));
        assert_eq!(db.record_hook_event(&evt).unwrap(), HookEventOutcome::New);
        assert_eq!(
            db.record_hook_event(&evt).unwrap(),
            HookEventOutcome::Duplicate,
            "same idempotency key is a Duplicate"
        );

        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM hook_events WHERE task_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "duplicate must not add a row");
    }

    #[test]
    fn record_hook_event_records_a_block_recompletion_stop() {
        // Real-machine regression (#131 follow-up): a Stop-hook `block` makes the
        // agent re-complete WITHIN THE SAME TURN, so the re-completion Stop shares
        // (job_id, session, prompt_id, event='stop') with the initial blank Stop
        // but carries a different status (UNKNOWN -> COMPLETED). It must be
        // recorded — not dropped as a re-delivery — or the completion is lost and
        // the task strands in `dispatched`.
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        let blank = HookEventInsert {
            prompt_id: "p1".into(),
            ..hook_event(id, "job-1-1", "stop", Some("UNKNOWN"))
        };
        let done = HookEventInsert {
            prompt_id: "p1".into(),
            ..hook_event(id, "job-1-1", "stop", Some("COMPLETED"))
        };

        assert_eq!(db.record_hook_event(&blank).unwrap(), HookEventOutcome::New);
        assert_eq!(
            db.record_hook_event(&done).unwrap(),
            HookEventOutcome::New,
            "a status change on the same key is a new signal, not a duplicate"
        );
        // But an identical re-delivery of the COMPLETED still dedups (idempotency
        // — a curl retry / spool re-send must not double-transition, F-#4).
        assert_eq!(
            db.record_hook_event(&done).unwrap(),
            HookEventOutcome::Duplicate,
            "an identical re-delivery of the same status is still a Duplicate"
        );

        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM hook_events WHERE task_id = ?1 AND event='stop'",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "the UNKNOWN and the COMPLETED are both recorded");
    }

    #[test]
    fn unknown_stop_streak_counts_trailing_unknowns() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // No stops yet.
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 0);

        // A COMPLETED stop keeps the streak at 0.
        db.record_hook_event(&hook_event(id, "job-1-1", "stop", Some("COMPLETED")))
            .unwrap();
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 0);

        // Two UNKNOWN stops -> 2. A non-stop event in between is ignored.
        db.record_hook_event(&hook_event(id, "job-1-2", "stop", Some("UNKNOWN")))
            .unwrap();
        db.record_hook_event(&hook_event(id, "job-1-3", "notification", None))
            .unwrap();
        db.record_hook_event(&hook_event(id, "job-1-4", "stop", Some("UNKNOWN")))
            .unwrap();
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 2);

        // A third UNKNOWN -> 3 (the escalation threshold, D-02).
        db.record_hook_event(&hook_event(id, "job-1-5", "stop", Some("UNKNOWN")))
            .unwrap();
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 3);

        // An interleaved COMPLETED resets the streak.
        db.record_hook_event(&hook_event(id, "job-1-6", "stop", Some("COMPLETED")))
            .unwrap();
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 0);

        // Fresh UNKNOWNs after the reset count from zero.
        db.record_hook_event(&hook_event(id, "job-1-7", "stop", Some("UNKNOWN")))
            .unwrap();
        assert_eq!(db.unknown_stop_streak(id).unwrap(), 1);
    }

    #[test]
    fn reserve_session_then_fill_native_id() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // Reserve returns a real row id (the job_id session_row) with an empty
        // native id, then the dispatch result fills it in.
        let row = db.reserve_session(id, "herdr").unwrap();
        let before = db.latest_session(id).unwrap().unwrap();
        assert_eq!(before.id, row);
        assert_eq!(
            before.session_id, "",
            "reserved row starts with no native id"
        );

        db.set_session_native_id(row, "cc-native").unwrap();
        let after = db.latest_session(id).unwrap().unwrap();
        assert_eq!(after.session_id, "cc-native");
        assert_eq!(after.plugin, "herdr");

        // Unknown ids are rejected, matching the other setters' contract.
        assert!(matches!(
            db.reserve_session(TaskId(999), "herdr").unwrap_err(),
            StateError::NotFound(TaskId(999))
        ));
        assert!(matches!(
            db.set_session_native_id(999, "x").unwrap_err(),
            StateError::SessionNotFound(999)
        ));
    }

    #[test]
    fn delete_session_rolls_back_a_reservation() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let row = db.reserve_session(id, "herdr").unwrap();
        assert!(db.latest_session(id).unwrap().is_some());

        // A failed dispatch rolls the reservation back → no session row remains.
        db.delete_session(row).unwrap();
        assert!(
            db.latest_session(id).unwrap().is_none(),
            "the reserved row must be gone after rollback"
        );
        // Deleting a missing row is a no-op (best-effort rollback), not an error.
        db.delete_session(row).unwrap();
    }

    #[test]
    fn tool_session_id_and_touch_last_signal() {
        let clock = manual_clock();
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let sess = db.record_session(id, "herdr", "sess-1").unwrap();

        // A fresh session has no Claude session id yet.
        assert_eq!(
            db.latest_session(id).unwrap().unwrap().tool_session_id,
            None
        );
        db.set_tool_session_id(sess, "cc-abc").unwrap();
        let rec = db
            .find_session_by_tool_session_id("cc-abc")
            .unwrap()
            .unwrap();
        assert_eq!(rec.id, sess);
        assert_eq!(rec.tool_session_id.as_deref(), Some("cc-abc"));
        assert!(
            db.find_session_by_tool_session_id("nope")
                .unwrap()
                .is_none()
        );

        // last_signal_at starts unset and gets stamped from the clock; a
        // later touch moves the anchor forward.
        assert!(db.get_task(id).unwrap().unwrap().last_signal_at.is_none());
        db.touch_last_signal(id).unwrap();
        assert_eq!(
            db.get_task(id)
                .unwrap()
                .unwrap()
                .last_signal_at
                .map(format_rfc3339)
                .as_deref(),
            Some(T0)
        );
        clock.advance(time::Duration::seconds(60));
        db.touch_last_signal(id).unwrap();
        assert_eq!(
            db.get_task(id)
                .unwrap()
                .unwrap()
                .last_signal_at
                .map(format_rfc3339)
                .as_deref(),
            Some("2026-01-01T00:01:00Z")
        );

        // Unknown ids are rejected, matching the other setters' contract.
        assert!(matches!(
            db.touch_last_signal(TaskId(999)).unwrap_err(),
            StateError::NotFound(TaskId(999))
        ));
        assert!(matches!(
            db.set_tool_session_id(999, "x").unwrap_err(),
            StateError::SessionNotFound(999)
        ));
    }
}
