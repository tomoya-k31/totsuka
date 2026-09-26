use rusqlite::{Row, params};
use time::OffsetDateTime;

use crate::domain::EventDetail;
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::task::{SourceTaskId, Task, TaskId};
use crate::domain::workflow::WorkflowMode;
use crate::ports::clock::{format_rfc3339, parse_rfc3339};

use super::{
    StateDb, StateError, TaskRef, apply_event_tx, conversion_error, unprocess_last_batch_tx,
};

/// Columns of `tasks`, read by name in [`row_to_task`].
const TASK_COLUMNS: &str = "id, source, source_task_id, workflow, mode, repo, \
     worktree_path, branch, base_commit, state, priority, title, url, source_payload, \
     finished_at, created_at, updated_at, last_signal_at, state_version";

/// A task to ingest (F-01). Starts life in [`TaskState::Queued`].
#[derive(Debug, Clone)]
pub struct NewTask {
    /// Source plugin instance name (e.g. `github`).
    pub source: String,
    /// The source's own task id (Issue number, Notion page id).
    pub source_task_id: SourceTaskId,
    /// Matched workflow name.
    pub workflow: String,
    /// Execution mode copied from the workflow.
    pub mode: WorkflowMode,
    /// Selected repository name (NULL while pending selection).
    pub repo: Option<String>,
    /// Priority; higher runs first.
    pub priority: i64,
    /// Human-readable title.
    pub title: String,
    /// Source URL.
    pub url: Option<String>,
    /// Residual source fields (labels/assignee/...) as JSON.
    pub source_payload: Option<serde_json::Value>,
    /// Timestamp of the last hook signal (R-10 timeout anchor). `None` until the
    /// first signal arrives; normally left unset at ingest.
    pub last_signal_at: Option<OffsetDateTime>,
}

/// Map a `tasks` row (in [`TASK_COLUMNS`] order) to a [`Task`].
fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    let state_str: String = row.get("state")?;
    let state = state_str
        .parse::<TaskState>()
        .map_err(|e| conversion_error(Box::new(e)))?;
    let mode_str: String = row.get("mode")?;
    let mode = mode_str
        .parse::<WorkflowMode>()
        .map_err(|e| conversion_error(Box::new(e)))?;
    let payload: Option<String> = row.get("source_payload")?;
    let source_payload = payload
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| conversion_error(Box::new(e)))?;

    Ok(Task {
        id: row.get("id")?,
        source: row.get("source")?,
        source_task_id: row.get("source_task_id")?,
        workflow: row.get("workflow")?,
        mode,
        repo: row.get("repo")?,
        worktree_path: row.get("worktree_path")?,
        branch: row.get("branch")?,
        base_commit: row.get("base_commit")?,
        state,
        priority: row.get("priority")?,
        title: row.get("title")?,
        url: row.get("url")?,
        source_payload,
        finished_at: optional_timestamp(row, "finished_at")?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
        last_signal_at: optional_timestamp(row, "last_signal_at")?,
        state_version: row.get("state_version")?,
    })
}

/// A stored timestamp that [`parse_rfc3339`] does not accept (#765).
///
/// Every timestamp column is written by [`format_rfc3339`], so this is a row
/// written by something else — a hand edit, another tool. Reading it is an
/// error, like an unknown state, rather than a silent `None`: a task whose
/// `finished_at` cannot be read must not have its worktree judged by a
/// retention policy, and one whose `last_signal_at` cannot be read must not
/// drop out of the timeout sweep.
#[derive(Debug, thiserror::Error)]
#[error("unreadable timestamp in `{column}`: {value:?} ({source})")]
pub struct BadTimestamp {
    /// The column it was read from.
    pub column: &'static str,
    /// The stored text.
    pub value: String,
    /// Why it did not parse.
    pub source: time::error::Parse,
}

fn timestamp(row: &Row<'_>, column: &'static str) -> rusqlite::Result<OffsetDateTime> {
    parse_timestamp(column, row.get(column)?)
}

fn optional_timestamp(
    row: &Row<'_>,
    column: &'static str,
) -> rusqlite::Result<Option<OffsetDateTime>> {
    row.get::<_, Option<String>>(column)?
        .map(|value| parse_timestamp(column, value))
        .transpose()
}

fn parse_timestamp(column: &'static str, value: String) -> rusqlite::Result<OffsetDateTime> {
    parse_rfc3339(&value).map_err(|source| {
        conversion_error(Box::new(BadTimestamp {
            column,
            value,
            source,
        }))
    })
}

impl StateDb {
    /// Ingest a task idempotently (F-73). Returns its id, whether newly
    /// inserted or already present under the same `(source, source_task_id)`.
    pub fn upsert_task(&self, task: &NewTask) -> Result<TaskId, StateError> {
        self.upsert_task_inner(task, &EventDetail::Ingested)
    }

    /// [`upsert_task`](Self::upsert_task) for the push path (`task/submit`,
    /// 0.1.6): identical semantics, but the ingest audit event records
    /// `{"kind":"submitted"}` so push and fetch ingests stay distinguishable.
    pub fn upsert_submitted_task(&self, task: &NewTask) -> Result<TaskId, StateError> {
        self.upsert_task_inner(task, &EventDetail::Submitted)
    }

    fn upsert_task_inner(
        &self,
        task: &NewTask,
        detail: &EventDetail,
    ) -> Result<TaskId, StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.to_json()?;
        let payload = task
            .source_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        // Insert + ingest event must be atomic so the audit log invariant
        // (F-72) holds even if the second write fails.
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "INSERT INTO tasks
                (source, source_task_id, workflow, mode, repo, state, priority,
                 title, url, source_payload, created_at, updated_at,
                 last_signal_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11,?12)
             ON CONFLICT(source, source_task_id) DO NOTHING",
            params![
                task.source,
                task.source_task_id,
                task.workflow,
                task.mode.as_str(),
                task.repo,
                TaskState::Queued.as_str(),
                task.priority,
                task.title,
                task.url,
                payload,
                now,
                task.last_signal_at.map(format_rfc3339),
            ],
        )?;
        let id: TaskId = tx.query_row(
            "SELECT id FROM tasks WHERE source = ?1 AND source_task_id = ?2",
            params![task.source, task.source_task_id],
            |r| r.get(0),
        )?;
        if changed > 0 {
            // Ingest event: from_state NULL -> queued (F-72). `detail` is JSON.
            tx.execute(
                "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
                 VALUES (?1, NULL, ?2, ?3, ?4)",
                params![id, TaskState::Queued.as_str(), now, detail],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Fetch a task by id.
    pub fn get_task(&self, id: TaskId) -> Result<Option<Task>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_task)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// Fetch a task by its source identity.
    pub fn find_by_source(
        &self,
        source: &str,
        source_task_id: &SourceTaskId,
    ) -> Result<Option<Task>, StateError> {
        let sql =
            format!("SELECT {TASK_COLUMNS} FROM tasks WHERE source = ?1 AND source_task_id = ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![source, source_task_id], row_to_task)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// All tasks, newest first.
    pub fn list_tasks(&self) -> Result<Vec<Task>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY id DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_task)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Tasks currently in `state` (used by `status` and slot rebuild).
    pub fn tasks_in_state(&self, state: TaskState) -> Result<Vec<Task>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE state = ?1 ORDER BY id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state.as_str()], row_to_task)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Apply a state-machine event to a task, recording an audit event.
    ///
    /// Sets `finished_at` on entering a terminal state and clears it otherwise
    /// (e.g. on retry). Returns the new state and the updated reference, or
    /// an error with the DB left unchanged: [`StateError::Conflict`] when the
    /// task moved since `task` was read (#763), [`StateError::Transition`]
    /// when the event is illegal in the state it was read in.
    pub fn apply_event(
        &self,
        task: TaskRef,
        event: TaskEvent,
        detail: Option<EventDetail>,
    ) -> Result<(TaskState, TaskRef), StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(EventDetail::to_json).transpose()?;
        // Update + audit event in one transaction: state never advances
        // without its recorded event (F-72).
        let tx = self.write_transaction()?;
        let (to, version) = apply_event_tx(
            &tx,
            &now,
            task.id,
            Some(task.version),
            event,
            detail.as_deref(),
        )?;
        tx.commit()?;
        Ok((
            to,
            TaskRef {
                id: task.id,
                version,
            },
        ))
    }

    /// A transaction that holds the write lock from its first statement
    /// (`BEGIN IMMEDIATE`), for a version check followed by a write (#763).
    ///
    /// A deferred transaction reads the version on a snapshot and only asks
    /// for the write lock at the `UPDATE`. If another connection committed in
    /// between, WAL refuses the upgrade with `SQLITE_BUSY_SNAPSHOT`, which the
    /// busy handler does not retry — the race would surface as a fatal
    /// [`StateError::Db`] instead of the [`StateError::Conflict`] it is.
    /// Taking the lock first makes the other writer wait (busy timeout) and
    /// this read see what it committed.
    fn write_transaction(&self) -> Result<rusqlite::Transaction<'_>, StateError> {
        Ok(rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?)
    }

    /// The current reference to task `id`, for a caller that decides on
    /// nothing but the id. A transition applied through it still fails if
    /// the task moves after this read.
    pub fn task_ref(&self, id: TaskId) -> Result<TaskRef, StateError> {
        self.get_task(id)?
            .map(|t| t.task_ref())
            .ok_or(StateError::NotFound(id))
    }

    /// Record the selected repository for a task (F-14 confirmation result).
    pub fn set_repo(&self, id: TaskId, repo: &str) -> Result<(), StateError> {
        let n = self.conn.execute(
            "UPDATE tasks SET repo = ?1, updated_at = ?2 WHERE id = ?3",
            params![repo, self.clock.now_rfc3339(), id],
        )?;
        if n == 0 {
            return Err(StateError::NotFound(id));
        }
        Ok(())
    }

    /// Record the worktree path, branch and base commit for a task (#53, v8).
    ///
    /// `branch` is optional: a worktree can exist without one, and the caller
    /// must be able to say so rather than inventing a name. `base_commit` is
    /// not — creation always resolves one, and cleanup needs it to tell this
    /// task's branch apart from the operator's.
    ///
    /// **`base_commit` is written once** (`COALESCE`). A task can be dispatched
    /// again after its worktree was cleaned up (#254), and `create` recomputes
    /// the base from a *fresh* `origin/{default}` every time. Overwriting would
    /// walk the recorded value forward past the commit the task's branch was
    /// actually cut from, and the ownership test
    /// (`merge-base --is-ancestor <base> <branch>`) would then answer "not
    /// ours" for a branch that is. It fails safe — the branch is kept — but
    /// that is exactly the unbounded accumulation #266 was about. The value
    /// means "where this task's work started", which happens once.
    pub fn set_worktree(
        &self,
        id: TaskId,
        path: &str,
        branch: Option<&str>,
        base_commit: &str,
    ) -> Result<(), StateError> {
        let n = self.conn.execute(
            "UPDATE tasks SET worktree_path = ?1, branch = ?2, \
             base_commit = COALESCE(base_commit, ?3), updated_at = ?4 WHERE id = ?5",
            params![path, branch, base_commit, self.clock.now_rfc3339(), id],
        )?;
        if n == 0 {
            return Err(StateError::NotFound(id));
        }
        Ok(())
    }

    /// Record the branch a task's worktree turned out to be on.
    ///
    /// Separate from [`set_worktree`](Self::set_worktree) because the branch is
    /// no longer known when the worktree is created: it is read back from
    /// `HEAD` after the agent has chosen and created it, which can be any
    /// number of ticks later.
    pub fn set_branch(&self, id: TaskId, branch: &str) -> Result<(), StateError> {
        let n = self.conn.execute(
            "UPDATE tasks SET branch = ?1, updated_at = ?2 WHERE id = ?3",
            params![branch, self.clock.now_rfc3339(), id],
        )?;
        if n == 0 {
            return Err(StateError::NotFound(id));
        }
        Ok(())
    }

    /// Forget the branch a task's worktree was on (#568).
    ///
    /// Handing a conversation to a **read-only** stage detaches its worktree
    /// so the stage is not blamed for a branch the previous one made. The
    /// detach alone is not enough: if the worktree is no longer on disk,
    /// `acquire_worktree` re-creates it from this column and puts it straight
    /// back on that branch, reproducing the very failure the detach avoids.
    /// Clearing the column makes re-creation hand over a detached worktree,
    /// which is what a read-only stage should start from.
    ///
    /// The branch itself is untouched — this forgets a pointer, not work. A
    /// later writing stage records whatever branch its agent chooses.
    pub fn clear_branch(&self, id: TaskId) -> Result<(), StateError> {
        self.conn.execute(
            "UPDATE tasks SET branch = NULL, updated_at = ?1 WHERE id = ?2",
            params![self.clock.now_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Requeue a task **and** put the batch of messages its failed run was
    /// given back on the queue, in one transaction (#242).
    ///
    /// Without the requeue, a retry after the agent failed would dispatch with
    /// nothing to say: the messages were stamped processed when they were
    /// handed over, and `task retry` is precisely the case where that handover
    /// did not work out. Atomic for the same reason as
    /// [`append_task_message_reopening`](Self::append_task_message_reopening) —
    /// a task requeued without its messages is a dispatch with an empty
    /// prompt, and nothing would ever notice.
    ///
    /// A conversation that *already* has unsent messages gets none back: the
    /// run being retried never received a batch (it died before dispatch), so
    /// there is nothing to take back — and the batch before it was answered
    /// already. Reviving that one would replay an answered instruction
    /// alongside the new one.
    ///
    /// Returns the new state, the updated reference and how many messages
    /// came back. Held to `task`'s version like [`apply_event`](Self::apply_event).
    pub fn retry_task(
        &self,
        task: TaskRef,
        detail: Option<EventDetail>,
    ) -> Result<(TaskState, TaskRef, usize), StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(EventDetail::to_json).transpose()?;
        let tx = self.write_transaction()?;
        let id = task.id;
        let (to, version) = apply_event_tx(
            &tx,
            &now,
            id,
            Some(task.version),
            TaskEvent::Retry,
            detail.as_deref(),
        )?;
        let already_queued: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_messages WHERE task_id = ?1 AND processed_at IS NULL",
            params![id],
            |r| r.get(0),
        )?;
        let requeued = if already_queued > 0 {
            0
        } else {
            unprocess_last_batch_tx(&tx, id)?
        };
        tx.commit()?;
        Ok((to, TaskRef { id, version }, requeued))
    }

    /// Bump a task's `last_signal_at` to now — the R-10 timeout anchor.
    pub fn touch_last_signal(&self, task_id: TaskId) -> Result<(), StateError> {
        let now = self.clock.now_rfc3339();
        let n = self.conn.execute(
            "UPDATE tasks SET last_signal_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, task_id],
        )?;
        if n == 0 {
            return Err(StateError::NotFound(task_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use rusqlite::Connection;

    use crate::adapters::state_db::test_support::*;
    use crate::adapters::state_db::{BadTimestamp, NOTE_KEY, TransitionConflict};
    use crate::domain::event_detail::Publish;
    use crate::ports::clock::Clock;

    #[test]
    fn ingest_is_idempotent() {
        let db = StateDb::open_in_memory().unwrap();
        let id1 = db.upsert_task(&sample_task()).unwrap();
        let id2 = db.upsert_task(&sample_task()).unwrap();
        assert_eq!(id1, id2, "duplicate ingest must return the same id (F-73)");
        assert_eq!(db.list_tasks().unwrap().len(), 1);
        // Only the first ingest records an event.
        assert_eq!(db.event_count(id1).unwrap(), 1);
    }

    #[test]
    fn event_transitions_and_audit_log() {
        let clock = manual_clock();
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        assert_eq!(
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
                .unwrap()
                .0,
            TaskState::Dispatched
        );
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Start, None)
            .unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::BeginPublish, None)
            .unwrap();
        clock.advance(time::Duration::seconds(90));
        let final_state = db
            .apply_event(
                db.task_ref(id).unwrap(),
                TaskEvent::Complete,
                Some(EventDetail::Publish(Publish::Succeeded {
                    policy: "source".to_string(),
                    pr_url: None,
                })),
            )
            .unwrap()
            .0;
        assert_eq!(final_state, TaskState::Done);

        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Done);
        assert_eq!(
            rec.finished_at.map(format_rfc3339).as_deref(),
            Some("2026-01-01T00:01:30Z"),
            "the terminal transition stamps finished_at from the clock"
        );
        assert_eq!(format_rfc3339(rec.created_at), T0);
        // 1 ingest + 4 transitions.
        assert_eq!(db.event_count(id).unwrap(), 5);
    }

    #[test]
    fn illegal_transition_leaves_db_unchanged() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        // Cannot Start straight from Queued.
        assert!(
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Start, None)
                .is_err()
        );
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Queued);
        assert_eq!(db.event_count(id).unwrap(), 1); // only ingest
    }

    /// The race in #763: the engine read the task, another writer cancelled
    /// it, and the engine's `Fail` lands on `Cancelled`. It must come back as
    /// a conflict — judged before the transition, which on its own would call
    /// this an illegal `Cancelled → Failed` — and write nothing.
    #[test]
    fn a_stale_reference_conflicts_and_writes_nothing() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let (_, engine_ref) = db
            .apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Cancel, None)
            .unwrap();
        let events = db.event_count(id).unwrap();

        let err = db
            .apply_event(engine_ref, TaskEvent::Fail, None)
            .unwrap_err();
        assert!(
            matches!(
                &err,
                StateError::Conflict(c) if *c == TransitionConflict {
                    id,
                    expected: 1,
                    actual: 2,
                    actual_state: TaskState::Cancelled,
                    event: TaskEvent::Fail,
                }
            ),
            "{err:?}"
        );
        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Cancelled);
        assert_eq!(rec.state_version, 2);
        assert_eq!(db.event_count(id).unwrap(), events);
    }

    /// Two connections, as in production (the engine and `task cancel`): the
    /// other writer commits while this transition is waiting to start. With a
    /// deferred transaction the version is read on a stale snapshot and the
    /// write fails as `SQLITE_BUSY_SNAPSHOT` — a DB error, fatal to the
    /// engine. Holding the write lock from the start turns it into the
    /// conflict it is.
    #[test]
    fn a_transition_racing_another_connection_reports_a_conflict() {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-two_writers", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let db = StateDb::open(&path).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let (_, engine_ref) = db
            .apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();

        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            other
                .execute(
                    "UPDATE tasks SET state = 'cancelled', state_version = state_version + 1 \
                     WHERE id = ?1",
                    params![id],
                )
                .unwrap();
            other.execute_batch("COMMIT").unwrap();
        });
        let err = db
            .apply_event(engine_ref, TaskEvent::Fail, None)
            .unwrap_err();
        writer.join().unwrap();
        assert!(matches!(err, StateError::Conflict { .. }), "{err:?}");
        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::Cancelled
        );
        drop(db);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Comparing states would not catch this: cancel → retry → re-dispatch
    /// leaves the task `Dispatched` again, the state the old attempt read.
    /// The version has moved on, so the old attempt's write is refused.
    #[test]
    fn a_task_back_in_the_state_it_was_read_in_still_conflicts() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let (_, first_attempt) = db
            .apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        for event in [TaskEvent::Cancel, TaskEvent::Retry, TaskEvent::Dispatch] {
            db.apply_event(db.task_ref(id).unwrap(), event, None)
                .unwrap();
        }
        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::Dispatched
        );

        let err = db
            .apply_event(first_attempt, TaskEvent::Fail, None)
            .unwrap_err();
        assert!(matches!(err, StateError::Conflict(_)), "{err:?}");
        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::Dispatched
        );
    }

    /// Only transitions move the version: a note is not a change of state, and
    /// counting it would turn every `status` note into a spurious conflict for
    /// the engine that is about to write the task's next transition.
    #[test]
    fn transitions_bump_the_version_and_notes_do_not() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        let before = db.task_ref(id).unwrap();
        assert_eq!(before.version(), 0);
        assert!(
            db.note_task(id, &serde_json::json!({ NOTE_KEY: "blocked" }))
                .unwrap()
        );
        let (_, after) = db.apply_event(before, TaskEvent::Dispatch, None).unwrap();
        assert_eq!(after.version(), 1);
        // The returned reference is the one to write through next.
        let (state, after) = db.apply_event(after, TaskEvent::Start, None).unwrap();
        assert_eq!((state, after.version()), (TaskState::Running, 2));
    }

    #[test]
    fn retry_is_held_to_the_version_too() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Fail, None)
            .unwrap();
        let stale = db.task_ref(id).unwrap();
        let (state, _, _) = db.retry_task(db.task_ref(id).unwrap(), None).unwrap();
        assert_eq!(state, TaskState::Queued);

        let err = db.retry_task(stale, None).unwrap_err();
        assert!(matches!(err, StateError::Conflict(_)), "{err:?}");
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Queued);
    }

    #[test]
    fn survives_reopen_from_disk() {
        // Kill-and-restart: write to a file, drop, reopen, expect state back.
        let dir = std::env::temp_dir().join(format!("totsuka-{}-state_reopen", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state.db");

        let id = {
            let db = StateDb::open(&path).unwrap();
            let id = db.upsert_task(&sample_task()).unwrap();
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
                .unwrap();
            db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Start, None)
                .unwrap();
            id
        }; // db dropped, simulating process exit

        let db = StateDb::open(&path).unwrap();
        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Running);
        assert_eq!(rec.source_task_id, SourceTaskId("42".into()));
        // Events survived too.
        assert_eq!(db.event_count(id).unwrap(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tasks_in_state_and_setters() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.set_repo(id, "totsuka").unwrap();
        db.set_worktree(id, "/tmp/wt", Some("agent/github-42"), "c0ffee")
            .unwrap();

        let queued = db.tasks_in_state(TaskState::Queued).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].repo.as_deref(), Some("totsuka"));
        assert_eq!(queued[0].branch.as_deref(), Some("agent/github-42"));
        assert_eq!(queued[0].base_commit.as_deref(), Some("c0ffee"));
        assert!(db.tasks_in_state(TaskState::Running).unwrap().is_empty());
    }

    /// Re-creation after a cleanup (#254) recomputes the base from a fresh
    /// `origin/{default}`, which is a *later* commit. Letting that overwrite
    /// the recorded value would move the task's starting point past the commit
    /// its branch was cut from, and cleanup's ownership test would stop
    /// recognising its own branch.
    #[test]
    fn the_base_commit_is_recorded_once_and_not_moved_by_a_re_creation() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.set_worktree(id, "/tmp/wt", None, "original").unwrap();
        // The same task, dispatched again after its worktree was cleaned up.
        db.set_worktree(id, "/tmp/wt", Some("feat/x"), "moved-on")
            .unwrap();

        let task = db.get_task(id).unwrap().unwrap();
        assert_eq!(task.base_commit.as_deref(), Some("original"));
        // Everything else still updates — only the base is pinned.
        assert_eq!(task.branch.as_deref(), Some("feat/x"));
    }

    /// A worktree can exist without being on a branch, and the record has to
    /// be able to say so — writing a placeholder name instead would hand
    /// cleanup a branch to go looking for (and possibly delete).
    #[test]
    fn a_worktree_can_be_recorded_without_a_branch() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.set_worktree(id, "/tmp/wt", None, "c0ffee").unwrap();

        let task = db.get_task(id).unwrap().unwrap();
        assert_eq!(task.worktree_path.as_deref(), Some("/tmp/wt"));
        assert_eq!(task.branch, None);
        assert_eq!(task.base_commit.as_deref(), Some("c0ffee"));
    }

    #[test]
    fn setters_reject_unknown_task() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(matches!(
            db.set_repo(TaskId(999), "totsuka").unwrap_err(),
            StateError::NotFound(TaskId(999))
        ));
        assert!(matches!(
            db.set_worktree(TaskId(999), "/tmp/wt", Some("b"), "c0ffee")
                .unwrap_err(),
            StateError::NotFound(TaskId(999))
        ));
    }

    #[test]
    fn a_task_timestamp_reads_back_as_the_bytes_it_was_stored_as() {
        // #765: `Task` holds `OffsetDateTime` and `--json` formats it again on
        // the way out, so parse → format must give back exactly what is in
        // the column — for every subsecond width the formatter writes (#478).
        let clock = manual_clock();
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        for step_ms in [0, 500, 30, 7] {
            clock.advance(time::Duration::milliseconds(step_ms));
            let mut task = sample_task();
            task.source_task_id = SourceTaskId(format!("ts-{step_ms}"));
            task.last_signal_at = Some(clock.now_utc());
            let id = db.upsert_task(&task).unwrap();
            let stored: (String, String) = db
                .conn
                .query_row(
                    "SELECT created_at, last_signal_at FROM tasks WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            let read = db.get_task(id).unwrap().unwrap();
            assert_eq!(format_rfc3339(read.created_at), stored.0);
            assert_eq!(read.last_signal_at.map(format_rfc3339), Some(stored.1));
        }
        // The widths above, spelled out: none, one, two and three digits.
        for text in [
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00.5Z",
            "2026-01-01T00:00:00.53Z",
            "2026-01-01T00:00:00.537Z",
        ] {
            assert_eq!(format_rfc3339(parse_rfc3339(text).unwrap()), text);
        }
    }

    #[test]
    fn an_unreadable_task_timestamp_is_an_error_not_a_missing_one() {
        // #765: before, a `last_signal_at` that would not parse dropped the
        // task out of the timeout sweep, and a bad `finished_at` kept its
        // worktree forever. Now the read fails, the way an unknown state does.
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.conn
            .execute(
                "UPDATE tasks SET last_signal_at = 'yesterday' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        let err = db.get_task(id).unwrap_err();
        let StateError::Db(rusqlite::Error::FromSqlConversionFailure(_, _, source)) = &err else {
            panic!("expected a conversion failure, got {err:?}");
        };
        let bad = source
            .downcast_ref::<BadTimestamp>()
            .expect("the source is a BadTimestamp");
        assert_eq!(bad.column, "last_signal_at");
        assert_eq!(bad.value, "yesterday");
    }

    #[test]
    fn an_unknown_task_mode_is_an_error_not_implement() {
        // #765: `mode` used to be read as a string and anything but "plan"
        // ran as implement. Now the row fails to read, like an unknown state:
        // a row nobody can vouch for must not start pushing branches.
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.conn
            .execute(
                "UPDATE tasks SET mode = 'design' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        let err = db.get_task(id).unwrap_err();
        let StateError::Db(rusqlite::Error::FromSqlConversionFailure(_, _, source)) = &err else {
            panic!("expected a conversion failure, got {err:?}");
        };
        assert_eq!(
            source.downcast_ref::<crate::domain::UnknownMode>(),
            Some(&crate::domain::UnknownMode("design".to_string()))
        );
    }

    /// A dispatch starts a new execution, so the D-03 silence anchor from the
    /// previous one must not survive it (#382). It did: a task that signalled,
    /// failed, and was retried minutes later was swept as "silent past
    /// `timeout_secs`" and escalated before its fresh agent could emit
    /// anything.
    #[test]
    fn dispatch_clears_the_previous_executions_signal_anchor() {
        let clock = manual_clock();
        let db = StateDb::open_in_memory_with_clock(clock.clone()).unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        // First execution: dispatched, and it proved itself alive.
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
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

        // …then it failed, and a human retried it much later.
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Fail, None)
            .unwrap();
        clock.advance(time::Duration::seconds(1600));
        db.retry_task(db.task_ref(id).unwrap(), None).unwrap();
        // The retry itself does not clear it — the anchor belongs to the
        // execution, and re-queueing has not started one yet.
        assert_eq!(
            db.get_task(id)
                .unwrap()
                .unwrap()
                .last_signal_at
                .map(format_rfc3339)
                .as_deref(),
            Some(T0)
        );

        // Dispatching does. Without this the sweep would compare `now` against
        // a 1600s-old anchor and escalate immediately.
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        assert_eq!(
            db.get_task(id).unwrap().unwrap().last_signal_at,
            None,
            "the new execution starts with no anchor, exactly like a first dispatch"
        );
    }
}
