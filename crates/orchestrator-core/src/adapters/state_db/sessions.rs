use rusqlite::{Row, params};

use crate::domain::task::TaskId;

use super::{StateDb, StateError};

/// Columns of `sessions`, read by name in [`row_to_session`].
const SESSION_COLUMNS: &str = "id, task_id, plugin, session_id, created_at, tool_session_id";

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

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use crate::adapters::state_db::test_support::*;
    use crate::domain::state::{TaskEvent, TaskState};
    use crate::ports::clock::format_rfc3339;

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
