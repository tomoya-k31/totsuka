use rusqlite::params;

use crate::domain::task::TaskId;

use super::{StateDb, StateError};

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

impl StateDb {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapters::state_db::test_support::*;
    use crate::adapters::state_db::{HookEventInsert, HookEventOutcome};

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
}
