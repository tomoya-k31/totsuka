use std::collections::HashMap;

use rusqlite::{OptionalExtension, Row, params};

use crate::domain::EventDetail;
use crate::domain::state::TaskState;
use crate::domain::task::{SourceTaskId, TaskId};

use super::{NOTE_KEY, StateDb, StateError, conversion_error};

/// A persisted audit event (F-72), for `task show` history.
///
/// Usually a state transition. A row whose `detail` carries [`NOTE_KEY`] is a
/// **note** instead — something recorded *about* a task that did not move
/// (#407) — and has `from_state == Some(to_state)`. See
/// [`StateDb::note_task`].
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    /// Row id.
    pub id: i64,
    /// Owning task id.
    pub task_id: TaskId,
    /// State before the transition (`None` for the ingest event; equal to
    /// [`to_state`](Self::to_state) for a note).
    pub from_state: Option<TaskState>,
    /// State after the transition — or the unchanged state, for a note.
    pub to_state: TaskState,
    /// Timestamp (ISO 8601 UTC).
    pub occurred_at: String,
    /// Structured detail, if recorded.
    pub detail: Option<serde_json::Value>,
}

/// Which events one `totsuka task export` walk should visit (#463).
#[derive(Debug, Default, Clone, Copy)]
pub struct EventExportFilter {
    /// Only events with `id` **strictly greater** than this — the cursor for
    /// an incremental export. `events` is append-only and `id` is a SQLite
    /// `INTEGER PRIMARY KEY`, so "the last id I saw" is a complete cursor.
    pub after_id: Option<i64>,
    /// Only events belonging to this task.
    pub task_id: Option<TaskId>,
    /// Skip the `detail` column entirely.
    ///
    /// Not a redaction feature — the same content is already reachable through
    /// `totsuka task show --json`. It exists because `detail` carries the
    /// agent's accumulated terminal output on the publish transitions
    /// (`publish_artifact`), which makes individual rows arbitrarily large;
    /// this drops them at the SQL level rather than after loading.
    pub without_detail: bool,
}

/// One exported audit event: the [`events`](EventRecord) row plus the owning
/// task's immutable identity, since an event alone cannot be interpreted
/// (#463).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExportedEvent {
    /// `events.id` — the cursor for [`EventExportFilter::after_id`].
    pub event_id: i64,
    /// Owning task id.
    pub task_id: TaskId,
    /// State before the transition; `null` for the ingest event.
    pub from: Option<&'static str>,
    /// State after the transition.
    pub to: &'static str,
    /// Timestamp (ISO 8601 UTC).
    pub occurred_at: String,
    /// Structured detail as recorded — with "suppressed" and "never recorded"
    /// kept apart, which is why this is doubly optional:
    ///
    /// | value | JSON | meaning |
    /// |---|---|---|
    /// | `None` | key absent | [`EventExportFilter::without_detail`] dropped it |
    /// | `Some(None)` | `"detail": null` | the row recorded no detail |
    /// | `Some(Some(v))` | `"detail": v` | as recorded |
    ///
    /// Collapsing the two into one absent key would leave an archive taken
    /// with `--no-detail` unable to say which transitions *had* a detail —
    /// answerable only by going back to the DB, which is the situation this
    /// export exists to avoid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Option<serde_json::Value>>,
    /// The owning task, denormalized.
    pub task: ExportedTask,
}

/// The owning task's immutable fields, carried on every [`ExportedEvent`].
///
/// Deliberately only the fields that never change after ingest: a mutable one
/// (`state`, `branch`, `repo`) would describe the task as it is *now* while
/// the event describes a moment in the past, which is exactly the confusion an
/// append-only export exists to avoid.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExportedTask {
    /// Source plugin instance name.
    pub source: String,
    /// Id within that source.
    pub source_task_id: SourceTaskId,
    /// Matched workflow name.
    pub workflow: String,
    /// Task title.
    pub title: String,
}

/// An unresolved note recorded against a task (#407).
///
/// See [`StateDb::note_task`] for what a note is and why it resolves itself.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskNote {
    /// The [`NOTE_KEY`] value, naming what kind of note this is.
    pub kind: String,
    /// The whole `detail` object, including [`NOTE_KEY`].
    pub detail: serde_json::Value,
    /// When the note was recorded (ISO 8601 UTC) — how long the task has been
    /// in this condition.
    pub since: String,
}

/// Map one joined `events`/`tasks` row for
/// [`StateDb::for_each_exported_event`] (#463).
///
/// Positional rather than by-name because the `detail` column is selected as a
/// literal `NULL` when `without_detail` is set, which leaves it unnamed.
fn row_to_exported_event(row: &Row<'_>, without_detail: bool) -> Result<ExportedEvent, StateError> {
    let from: Option<String> = row.get(2)?;
    let from = from.map(|s| s.parse::<TaskState>()).transpose()?;
    let to: String = row.get(3)?;
    let to = to.parse::<TaskState>()?;
    let stored: Option<String> = row.get(5)?;
    // Parsed rather than passed through as a string, matching
    // `task show --json`'s `detail`. Deliberately **not** re-interpreted: the
    // `kind` vocabulary has grown over time, and reading old rows under
    // today's meanings is the exact failure an append-only export exists to
    // prevent.
    //
    // The parse is also byte-stable for rows totsuka wrote, which is what an
    // audit chain hashing exported lines needs: writes go through
    // `serde_json::Value` too, so the column already holds this crate's
    // canonical form (sorted keys, deduplicated) and the round-trip is
    // identity. `export_detail_round_trips_byte_for_byte` pins that, since it
    // holds by construction rather than by promise.
    let detail = stored.map(|s| serde_json::from_str(&s)).transpose()?;
    // `None` = suppressed, `Some(None)` = the row had none. The SQL already
    // selected a literal NULL under `without_detail`, so `detail` is `None`
    // either way here and only the flag can tell the two apart.
    let detail = if without_detail { None } else { Some(detail) };
    Ok(ExportedEvent {
        event_id: row.get(0)?,
        task_id: row.get(1)?,
        from: from.map(TaskState::as_str),
        to: to.as_str(),
        occurred_at: row.get(4)?,
        detail,
        task: ExportedTask {
            source: row.get(6)?,
            source_task_id: row.get(7)?,
            workflow: row.get(8)?,
            title: row.get(9)?,
        },
    })
}

impl StateDb {
    /// All audit events for a task, oldest first (F-72; `task show` history).
    pub fn list_events(&self, task_id: TaskId) -> Result<Vec<EventRecord>, StateError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, from_state, to_state, occurred_at, detail \
             FROM events WHERE task_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![task_id], |row| {
            let from: Option<String> = row.get("from_state")?;
            let from_state = from
                .map(|s| s.parse::<TaskState>())
                .transpose()
                .map_err(|e| conversion_error(Box::new(e)))?;
            let to: String = row.get("to_state")?;
            let to_state = to
                .parse::<TaskState>()
                .map_err(|e| conversion_error(Box::new(e)))?;
            let detail: Option<String> = row.get("detail")?;
            let detail = detail
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| conversion_error(Box::new(e)))?;
            Ok(EventRecord {
                id: row.get("id")?,
                task_id: row.get("task_id")?,
                from_state,
                to_state,
                occurred_at: row.get("occurred_at")?,
                detail,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Walk the audit log across **all** tasks, oldest first, handing each row
    /// to `sink` as it is read (#463).
    ///
    /// This is the flat-text escape hatch for a state of record that lives in
    /// SQLite: `events` is append-only and therefore already shaped like a log,
    /// but nothing could read it without `sqlite3` and knowledge of the schema.
    /// [`list_events`](Self::list_events) answers a different question (one
    /// task, as a `Vec`) and cannot serve this one.
    ///
    /// # Why a callback and not `-> Vec<_>` or `-> impl Iterator`
    ///
    /// **Streaming is a correctness requirement here, not a nicety.** A single
    /// `detail` can hold `publish_artifact` — the agent's whole accumulated
    /// terminal output — so a `Vec` of every event is unbounded in a way
    /// `list_events` (one task) never is. An `impl Iterator` would have to
    /// borrow the prepared statement, which cannot outlive this method; a
    /// callback keeps the statement alive for exactly the walk and never holds
    /// more than one row.
    ///
    /// `sink` returning `Err` aborts the walk and propagates — a closed pipe
    /// downstream should stop the query, not read the rest of the table into a
    /// buffer nobody will drain. Its error type is the caller's, so a consumer
    /// writing to stdout can keep its own I/O errors intact (`BrokenPipe` in
    /// particular) instead of flattening them into [`StateError::Io`].
    pub fn for_each_exported_event<F, E>(
        &self,
        filter: EventExportFilter,
        mut sink: F,
    ) -> Result<(), E>
    where
        F: FnMut(ExportedEvent) -> Result<(), E>,
        E: From<StateError>,
    {
        // `detail` is selected as a literal NULL rather than omitted from the
        // column list, so the row mapper below stays one shape.
        let detail_column = if filter.without_detail {
            "NULL"
        } else {
            "e.detail"
        };
        // Every DB error is mapped explicitly rather than via `?`: the return
        // type is the caller's `E`, so `From<rusqlite::Error>` is not in scope.
        let db = |e: rusqlite::Error| E::from(StateError::from(e));
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT e.id, e.task_id, e.from_state, e.to_state, e.occurred_at, \
                 {detail_column}, t.source, t.source_task_id, t.workflow, t.title \
                 FROM events e JOIN tasks t ON t.id = e.task_id \
                 WHERE (?1 IS NULL OR e.id > ?1) AND (?2 IS NULL OR e.task_id = ?2) \
                 ORDER BY e.id"
            ))
            .map_err(db)?;
        let mut rows = stmt
            .query(params![filter.after_id, filter.task_id])
            .map_err(db)?;
        while let Some(row) = rows.next().map_err(db)? {
            sink(row_to_exported_event(row, filter.without_detail).map_err(E::from)?)?;
        }
        Ok(())
    }

    /// Record a **note** against a task — something an operator needs to know
    /// about a task that is not moving — without moving it (#407).
    ///
    /// `detail` must be a JSON object whose [`NOTE_KEY`] field names the kind.
    /// The row is written to `events` with `from_state == to_state == ` the
    /// task's current state.
    ///
    /// # Why `events` and not a column on `tasks`
    ///
    /// **It resolves itself.** Every state transition writes an event (F-72),
    /// so the instant the task moves — dispatched, cancelled, failed — the
    /// note stops being the latest event and [`StateDb::open_notes`] stops
    /// reporting it. A `blocked_reason` column would have to be cleared by
    /// hand on every path out of the state, and the one path someone forgets
    /// is a `totsuka status` that lies about it forever.
    ///
    /// # Dedup
    ///
    /// Deduped against the task's **own history**, not the caller's memory:
    /// writes nothing and returns `false` when the task's latest event is
    /// already an identical note. That survives a restart, which an in-process
    /// set does not. It is deliberately a different question from "should we
    /// notify again" — after a restart the operator may never have seen the
    /// first notification, so the two dedups are kept separate.
    pub fn note_task(&self, id: TaskId, detail: &serde_json::Value) -> Result<bool, StateError> {
        // Enforced, not asserted: a `debug_assert!` is gone in release, and
        // the row it would let through is worse than useless — it becomes the
        // latest event and hides the note that was already there.
        if detail.get(NOTE_KEY).and_then(|v| v.as_str()).is_none() {
            return Err(StateError::NotANote(detail.to_string()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let state: Option<String> = tx
            .query_row("SELECT state FROM tasks WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        let state: TaskState = state.ok_or(StateError::NotFound(id))?.parse()?;
        let latest: Option<String> = tx
            .query_row(
                "SELECT detail FROM events WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if let Some(latest) = latest
            && serde_json::from_str::<serde_json::Value>(&latest)
                .ok()
                .as_ref()
                == Some(detail)
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
             VALUES (?1, ?2, ?2, ?3, ?4)",
            params![
                id,
                state.as_str(),
                self.clock.now_rfc3339(),
                serde_json::to_string(detail)?
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Every task whose latest event is a note, keyed by task id (#407).
    ///
    /// One query for the whole table rather than one per task: `totsuka
    /// status` renders every row and has a 500 ms budget (§5.5).
    pub fn open_notes(&self) -> Result<HashMap<TaskId, TaskNote>, StateError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.task_id, e.detail, e.occurred_at FROM events e \
             JOIN (SELECT task_id, MAX(id) AS max_id FROM events GROUP BY task_id) m \
               ON e.id = m.max_id \
             WHERE e.detail IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let detail: String = row.get("detail")?;
            Ok((
                row.get::<_, TaskId>("task_id")?,
                detail,
                row.get::<_, String>("occurred_at")?,
            ))
        })?;
        let mut notes = HashMap::new();
        for row in rows {
            let (task_id, detail, since) = row?;
            // A `detail` that will not parse is a row written by some other
            // version; it is not a note, and status is not the place to fail
            // over it.
            let Ok(detail) = serde_json::from_str::<serde_json::Value>(&detail) else {
                continue;
            };
            let Some(kind) = detail.get(NOTE_KEY).and_then(|v| v.as_str()) else {
                continue;
            };
            let kind = kind.to_string();
            notes.insert(
                task_id,
                TaskNote {
                    kind,
                    detail,
                    since,
                },
            );
        }
        Ok(notes)
    }

    /// Count of audit events recorded for a task (F-72).
    pub fn event_count(&self, id: TaskId) -> Result<i64, StateError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE task_id = ?1",
            params![id],
            |r| r.get(0),
        )?)
    }

    /// How many times the **current** run of dispatch failures has already
    /// been retried automatically (#492).
    ///
    /// Recomputed from the event log rather than held in the engine, for the
    /// same reason as [`unknown_stop_streak`](Self::unknown_stop_streak): a
    /// counter in memory is lost when `totsuka run` restarts, and this one has
    /// to survive that — a task that failed twice before a restart must not
    /// get a fresh budget of three.
    ///
    /// Scans events id-descending and counts the leading run of automatic
    /// requeues. Two things end the run, and both mean "whatever failed before
    /// this is not what we are counting now":
    ///
    /// - a **successful dispatch** (`to_state = dispatched`), and
    /// - any other way back to `queued` — a human's `totsuka task retry`
    ///   (`detail.kind = "cli"`), the first submission, a reopen. A person
    ///   stepping in resets the budget, which is what makes the automatic
    ///   retries and the deliberate one compose instead of competing.
    ///
    /// Rows that are neither (the `Fail` that precedes each requeue) are
    /// skipped, so the walk is ~O(streak) — the early `break` is what bounds
    /// it, and the `ORDER BY id DESC` costs no sort to get there. That last
    /// part is worth pinning because `idx_events_task` is on `(task_id)` alone
    /// and looks like it should need one: a SQLite index carries the rowid as
    /// its implicit tail, so the entries for one `task_id` are already in `id`
    /// order and the range is simply walked backwards. Measured — the plan is
    /// `SEARCH events USING INDEX idx_events_task (task_id=?)` with **no**
    /// `USE TEMP B-TREE FOR ORDER BY`.
    pub fn auto_retry_streak(&self, task_id: TaskId) -> Result<u32, StateError> {
        let mut stmt = self
            .conn
            .prepare("SELECT to_state, detail FROM events WHERE task_id = ?1 ORDER BY id DESC")?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut streak = 0u32;
        for row in rows {
            let (to_state, detail) = row?;
            if to_state == TaskState::Dispatched.as_str() {
                break;
            }
            if to_state != TaskState::Queued.as_str() {
                continue;
            }
            // A row that does not read as an `EventDetail` — another
            // version's kind, or a shape no writer produces — is not an
            // automatic retry, and ends the run like any other requeue (#766).
            let is_auto = detail
                .as_deref()
                .and_then(|d| serde_json::from_str::<EventDetail>(d).ok())
                .is_some_and(|d| matches!(d, EventDetail::AutoRetry { .. }));
            if is_auto {
                streak += 1;
            } else {
                break;
            }
        }
        Ok(streak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapters::state_db::test_support::*;
    use crate::adapters::state_db::{EventExportFilter, ExportedEvent, NewTask};
    use crate::domain::event_detail::{Cli, Dispatch, HookStart};
    use crate::domain::state::TaskEvent;

    /// Append an `events` row whose `detail` is `detail`'s JSON, verbatim.
    ///
    /// The write API only takes an [`EventDetail`] (#766), so a row no
    /// current writer produces — one from an older version, or one shaped to
    /// stress a reader — can only be written underneath it. The column gets
    /// the same bytes `apply_event` wrote for a `serde_json::Value` before
    /// #766.
    fn insert_raw_event(db: &StateDb, task_id: i64, to: TaskState, detail: &serde_json::Value) {
        db.conn
            .execute(
                "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
                 VALUES (?1, NULL, ?2, ?3, ?4)",
                params![
                    task_id,
                    to.as_str(),
                    T0,
                    serde_json::to_string(detail).unwrap()
                ],
            )
            .unwrap();
    }

    /// Seed two tasks with a few events each; returns their ids.
    fn seed_export_fixture(db: &StateDb) -> (i64, i64) {
        let first = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(db.task_ref(first).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        db.apply_event(
            db.task_ref(first).unwrap(),
            TaskEvent::Start,
            Some(EventDetail::Dispatch(Dispatch::Started {
                plugin: "herdr".to_string(),
                session_id: "s-1".to_string(),
            })),
        )
        .unwrap();

        let mut other = sample_task();
        other.source_task_id = SourceTaskId("43".to_string());
        other.title = "Another".to_string();
        let second = db.upsert_task(&other).unwrap();
        db.apply_event(
            db.task_ref(second).unwrap(),
            TaskEvent::Fail,
            Some(EventDetail::Hook { reason: None }),
        )
        .unwrap();
        (first.0, second.0)
    }

    fn collect_export(db: &StateDb, filter: EventExportFilter) -> Vec<ExportedEvent> {
        let mut out = Vec::new();
        db.for_each_exported_event::<_, StateError>(filter, |e| {
            out.push(e);
            Ok(())
        })
        .unwrap();
        out
    }

    /// The export walks **every** task in `events.id` order and carries the
    /// owning task's identity — the two things `list_events` cannot do (#463).
    #[test]
    fn export_walks_all_tasks_in_id_order_with_task_identity() {
        let db = StateDb::open_in_memory().unwrap();
        let (first, second) = seed_export_fixture(&db);

        let all = collect_export(&db, EventExportFilter::default());
        let ids: Vec<i64> = all.iter().map(|e| e.event_id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "oldest first, by event id");
        assert!(
            all.iter().any(|e| e.task_id == TaskId(first))
                && all.iter().any(|e| e.task_id == TaskId(second)),
            "both tasks appear: {all:?}"
        );

        let ingest = &all[0];
        assert_eq!(ingest.from, None, "the ingest event has no prior state");
        assert_eq!(ingest.task.source, "github");
        assert_eq!(ingest.task.source_task_id, SourceTaskId("42".into()));
        assert_eq!(ingest.task.workflow, "implement");
        assert_eq!(ingest.task.title, "Fix the bug");
    }

    /// `after_id` is a complete cursor: `events` is append-only, so "the last
    /// id I saw" resumes exactly where the previous export stopped.
    #[test]
    fn export_since_resumes_after_the_cursor() {
        let db = StateDb::open_in_memory().unwrap();
        seed_export_fixture(&db);

        let all = collect_export(&db, EventExportFilter::default());
        let cursor = all[1].event_id;
        let rest = collect_export(
            &db,
            EventExportFilter {
                after_id: Some(cursor),
                ..Default::default()
            },
        );
        assert_eq!(
            rest.iter().map(|e| e.event_id).collect::<Vec<_>>(),
            all[2..].iter().map(|e| e.event_id).collect::<Vec<_>>(),
            "strictly greater than the cursor, no overlap and no gap"
        );
    }

    /// `task_id` narrows to one task; `without_detail` drops the column.
    #[test]
    fn export_filters_by_task_and_can_drop_detail() {
        let db = StateDb::open_in_memory().unwrap();
        let (first, _) = seed_export_fixture(&db);

        let mine = collect_export(
            &db,
            EventExportFilter {
                task_id: Some(TaskId(first)),
                ..Default::default()
            },
        );
        assert!(
            mine.iter().all(|e| e.task_id == TaskId(first)) && !mine.is_empty(),
            "only the requested task: {mine:?}"
        );
        assert!(
            mine.iter().any(|e| matches!(&e.detail, Some(Some(_)))),
            "detail is present by default, matching `task show --json`: {mine:?}"
        );
        // A row that recorded nothing is `Some(None)` — reported as recorded,
        // and empty — never `None`, which means "suppressed".
        assert!(
            mine.iter().all(|e| e.detail.is_some()),
            "without --no-detail, every row reports its detail slot: {mine:?}"
        );

        let lean = collect_export(
            &db,
            EventExportFilter {
                task_id: Some(TaskId(first)),
                without_detail: true,
                ..Default::default()
            },
        );
        assert_eq!(
            lean.len(),
            mine.len(),
            "dropping detail drops no rows: {lean:?}"
        );
        assert!(
            lean.iter().all(|e| e.detail.is_none()),
            "no detail survives: {lean:?}"
        );

        // The two states must be distinguishable in the JSON, or an archive
        // taken with `--no-detail` cannot say which transitions had one.
        let suppressed = serde_json::to_value(&lean[0]).unwrap();
        assert!(
            suppressed.get("detail").is_none(),
            "--no-detail omits the key: {suppressed}"
        );
        let recorded_none = mine
            .iter()
            .find(|e| matches!(&e.detail, Some(None)))
            .expect("the ingest event records no detail");
        assert_eq!(
            serde_json::to_value(recorded_none).unwrap()["detail"],
            serde_json::Value::Null,
            "a row that recorded nothing emits an explicit null"
        );
    }

    /// The exported `detail` is byte-identical to the stored column.
    ///
    /// It holds by construction — writes go through `serde_json::Value` too,
    /// so the column already carries this crate's canonical form and the
    /// export's round-trip is identity — but "by construction" is exactly the
    /// kind of property that stops being true without anyone noticing. An
    /// audit chain hashing exported lines depends on it, so it is pinned here
    /// rather than promised in a comment.
    #[test]
    fn export_detail_round_trips_byte_for_byte() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        // Keys deliberately out of alphabetical order, plus a nested object
        // and a number, since those are where a reshape would show up. No
        // writer produces this shape, so it goes in underneath the typed API.
        insert_raw_event(
            &db,
            id.0,
            TaskState::Dispatched,
            &serde_json::json!({
                "kind": "hook_complete",
                "publish_artifact": "line one\nline two",
                "attempt": 3,
                "agent": {"plugin": "herdr", "session": "s-1"},
            }),
        );

        // Compare **every** row against its own stored column, keyed by
        // event id: picking one row by hand is how this test first passed
        // against the wrong event.
        let mut stmt = db
            .conn
            .prepare("SELECT id, detail FROM events WHERE detail IS NOT NULL ORDER BY id")
            .unwrap();
        let stored: HashMap<i64, String> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(stored.len() >= 2, "fixture has several details: {stored:?}");

        let mut compared = 0;
        for event in collect_export(&db, EventExportFilter::default()) {
            let Some(Some(value)) = &event.detail else {
                continue;
            };
            assert_eq!(
                serde_json::to_string(value).unwrap(),
                stored[&event.event_id],
                "event {}: the export must reproduce the stored bytes, not \
                 merely an equivalent document",
                event.event_id
            );
            compared += 1;
        }
        assert_eq!(compared, stored.len(), "every stored detail was checked");
    }

    /// A sink error aborts the walk instead of draining the table into a
    /// buffer nobody will read — this is what makes a closed pipe cheap.
    #[test]
    fn export_stops_when_the_sink_fails() {
        let db = StateDb::open_in_memory().unwrap();
        seed_export_fixture(&db);

        let mut seen = 0;
        let result =
            db.for_each_exported_event::<_, StateError>(EventExportFilter::default(), |_| {
                seen += 1;
                Err(StateError::NotFound(TaskId(-1)))
            });
        assert!(result.is_err(), "the sink's error propagates");
        assert_eq!(seen, 1, "and the walk stopped at the first row");
    }

    #[test]
    fn list_events_returns_full_history_in_order() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        db.apply_event(
            db.task_ref(id).unwrap(),
            TaskEvent::Start,
            Some(EventDetail::HookStart(HookStart::Plain {})),
        )
        .unwrap();

        let events = db.list_events(id).unwrap();
        assert_eq!(events.len(), 3);
        // Ingest first: no from_state, lands in queued.
        assert_eq!(events[0].from_state, None);
        assert_eq!(events[0].to_state, TaskState::Queued);
        assert_eq!(events[1].from_state, Some(TaskState::Queued));
        assert_eq!(events[1].to_state, TaskState::Dispatched);
        assert_eq!(events[2].to_state, TaskState::Running);
        assert_eq!(
            events[2].detail,
            Some(serde_json::json!({"kind": "hook_start"}))
        );
        // Unknown task -> empty history, not an error.
        assert!(db.list_events(TaskId(999)).unwrap().is_empty());
    }

    /// A note for `id`, shaped the way `run` writes one (#407).
    fn blocked_note() -> serde_json::Value {
        serde_json::json!({ NOTE_KEY: "blocked_agent_tools", "missing": ["gh"] })
    }

    #[test]
    fn a_note_records_once_and_resolves_when_the_task_moves() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        assert!(db.note_task(id, &blocked_note()).unwrap(), "first write");
        let note = db.open_notes().unwrap().remove(&id).expect("unresolved");
        assert_eq!(note.kind, "blocked_agent_tools");
        assert_eq!(note.detail, blocked_note());

        // The dispatch loop reaches this every cycle. Nothing accumulates,
        // and the note stays readable.
        for _ in 0..5 {
            assert!(!db.note_task(id, &blocked_note()).unwrap(), "deduped");
        }
        assert_eq!(db.event_count(id).unwrap(), 2, "ingest + one note");
        assert!(db.open_notes().unwrap().contains_key(&id));

        // The note is not a transition: the task is still queued and still
        // dispatchable.
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Queued);
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        assert!(
            !db.open_notes().unwrap().contains_key(&id),
            "moving the task resolves the note with no resolution record"
        );
    }

    #[test]
    fn a_note_is_recorded_again_after_the_condition_returns() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.note_task(id, &blocked_note()).unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Fail, None)
            .unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Retry, None)
            .unwrap();

        assert!(
            db.note_task(id, &blocked_note()).unwrap(),
            "the dedup is against the *latest* event, so a second wait is \
             recorded rather than swallowed"
        );
        assert!(db.open_notes().unwrap().contains_key(&id));
    }

    #[test]
    fn a_changed_note_supersedes_the_old_one() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.note_task(id, &blocked_note()).unwrap();
        let widened =
            serde_json::json!({ NOTE_KEY: "blocked_agent_tools", "missing": ["gh", "x"] });
        assert!(db.note_task(id, &widened).unwrap(), "detail differs");
        assert_eq!(db.open_notes().unwrap()[&id].detail, widened);
    }

    #[test]
    fn a_transition_detail_is_not_mistaken_for_a_note() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        // Every transition detail carries `kind`; only a note carries `note`.
        db.apply_event(
            db.task_ref(id).unwrap(),
            TaskEvent::Dispatch,
            Some(EventDetail::Dispatch(Dispatch::Started {
                plugin: "herdr".to_string(),
                session_id: "s-1".to_string(),
            })),
        )
        .unwrap();
        assert!(db.open_notes().unwrap().is_empty());
        // Nor is the ingest event, which is the latest one for a fresh task.
        let other = db
            .upsert_task(&NewTask {
                source_task_id: SourceTaskId("43".to_string()),
                ..sample_task()
            })
            .unwrap();
        assert!(!db.open_notes().unwrap().contains_key(&other));
    }

    #[test]
    fn noting_an_unknown_task_is_an_error_not_an_orphan_row() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(matches!(
            db.note_task(TaskId(999), &blocked_note()),
            Err(StateError::NotFound(TaskId(999)))
        ));
    }

    #[test]
    fn a_detail_without_the_note_key_is_refused_not_written() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.note_task(id, &blocked_note()).unwrap();

        for bad in [
            serde_json::json!({"kind": "dispatch"}),
            serde_json::json!({NOTE_KEY: 7}),
            serde_json::json!("blocked"),
        ] {
            assert!(matches!(
                db.note_task(id, &bad),
                Err(StateError::NotANote(_))
            ));
        }
        // The point of refusing: an unmarked row would be the latest event
        // and would hide the real note without anyone noticing.
        assert!(db.open_notes().unwrap().contains_key(&id));
        assert_eq!(db.event_count(id).unwrap(), 2, "ingest + the one real note");
    }

    /// Fail `id` and requeue it the way the engine does after a failed
    /// dispatch, recording `detail` on the requeue.
    fn fail_and_requeue(db: &StateDb, id: i64, detail: EventDetail) {
        db.apply_event(db.task_ref(TaskId(id)).unwrap(), TaskEvent::Fail, None)
            .unwrap();
        db.retry_task(db.task_ref(TaskId(id)).unwrap(), Some(detail))
            .unwrap();
    }

    fn auto_retry(attempt: u32) -> EventDetail {
        EventDetail::AutoRetry { attempt, limit: 3 }
    }

    #[test]
    fn auto_retry_streak_counts_the_trailing_automatic_requeues() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        // One before a successful dispatch, which ends the run.
        fail_and_requeue(&db, id.0, auto_retry(1));
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        fail_and_requeue(&db, id.0, auto_retry(1));
        fail_and_requeue(&db, id.0, auto_retry(2));
        assert_eq!(db.auto_retry_streak(id).unwrap(), 2);

        // A human's retry resets the budget.
        let cli = EventDetail::Cli(Cli::Plain {
            command: "task retry".to_string(),
        });
        fail_and_requeue(&db, id.0, cli);
        assert_eq!(db.auto_retry_streak(id).unwrap(), 0);
        fail_and_requeue(&db, id.0, auto_retry(1));
        assert_eq!(db.auto_retry_streak(id).unwrap(), 1);
    }

    /// A requeue whose detail does not read as an [`EventDetail`] — another
    /// version's kind, an `auto_retry` missing its fields, not an object at
    /// all — is not an automatic retry, and ends the run rather than failing
    /// the read (#766).
    #[test]
    fn auto_retry_streak_does_not_count_rows_it_cannot_read() {
        for row in [
            serde_json::json!({"kind": "from_the_future"}),
            serde_json::json!({"kind": "auto_retry"}),
            serde_json::json!("auto_retry"),
        ] {
            let db = StateDb::open_in_memory().unwrap();
            let id = db.upsert_task(&sample_task()).unwrap();
            fail_and_requeue(&db, id.0, auto_retry(1));
            assert_eq!(db.auto_retry_streak(id).unwrap(), 1);
            insert_raw_event(&db, id.0, TaskState::Queued, &row);
            assert_eq!(db.auto_retry_streak(id).unwrap(), 0, "{row}");
        }
    }

    #[test]
    fn every_event_detail_is_valid_json_or_null() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(db.task_ref(id).unwrap(), TaskEvent::Dispatch, None)
            .unwrap();
        db.apply_event(
            db.task_ref(id).unwrap(),
            TaskEvent::Start,
            Some(EventDetail::HookStart(HookStart::Plain {})),
        )
        .unwrap();

        // Read raw detail strings back and parse each as JSON (ingest + 2).
        let mut stmt = db
            .conn
            .prepare("SELECT detail FROM events WHERE task_id = ?1")
            .unwrap();
        let details: Vec<Option<String>> = stmt
            .query_map(params![id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(details.len(), 3);
        for s in details.into_iter().flatten() {
            serde_json::from_str::<serde_json::Value>(&s)
                .unwrap_or_else(|_| panic!("detail not valid JSON: {s:?}"));
        }
    }
}
