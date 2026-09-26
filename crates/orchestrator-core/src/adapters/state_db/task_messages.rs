use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::domain::EventDetail;
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::task::TaskId;
use crate::domain::workflow::WorkflowMode;

use super::{StateDb, StateError, TaskRef, apply_event_tx, unprocess_last_batch_tx};

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

impl StateDb {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapters::state_db::test_support::*;
    use crate::adapters::state_db::{NewTask, TaskMessageOutcome};
    use crate::domain::event_detail::Reopen;
    use crate::domain::task::SourceTaskId;

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
}
