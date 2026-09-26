use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::adapters::clock::SystemClock;
use crate::ports::clock::Clock;

use super::{StateDb, StateError};

/// Ordered, immutable schema migrations. Index + 1 is the version number.
const MIGRATIONS: &[&str] = &[
    // v1 — initial schema.
    r#"
    CREATE TABLE tasks (
      id              INTEGER PRIMARY KEY,
      source          TEXT NOT NULL,
      source_task_id  TEXT NOT NULL,
      workflow        TEXT NOT NULL,
      mode            TEXT NOT NULL,
      repo            TEXT,
      worktree_path   TEXT,
      branch          TEXT,
      state           TEXT NOT NULL,
      priority        INTEGER NOT NULL DEFAULT 0,
      title           TEXT NOT NULL,
      url             TEXT,
      source_payload  TEXT,
      finished_at     TEXT,
      created_at      TEXT NOT NULL,
      updated_at      TEXT NOT NULL,
      UNIQUE (source, source_task_id)
    );
    CREATE INDEX idx_tasks_state ON tasks(state);

    CREATE TABLE sessions (
      id          INTEGER PRIMARY KEY,
      task_id     INTEGER NOT NULL REFERENCES tasks(id),
      plugin      TEXT NOT NULL,
      session_id  TEXT NOT NULL,
      created_at  TEXT NOT NULL
    );
    CREATE INDEX idx_sessions_task ON sessions(task_id, created_at DESC);

    CREATE TABLE events (
      id          INTEGER PRIMARY KEY,
      task_id     INTEGER NOT NULL REFERENCES tasks(id),
      from_state  TEXT,
      to_state    TEXT NOT NULL,
      occurred_at TEXT NOT NULL,
      detail      TEXT
    );
    CREATE INDEX idx_events_task ON events(task_id);
    "#,
    // v2 — hook signals (#131/#134): idempotent hook-event log (D-05), audit
    // trail (N-01), and conversation-continuation correlation (E-09).
    //
    // The UNIQUE key's optional components default to '' (empty string), never
    // NULL: SQLite treats NULLs as distinct in UNIQUE constraints, so a NULL
    // default would let duplicate hook deliveries slip past the dedup.
    r#"
    ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;
    CREATE INDEX idx_sessions_claude_session ON sessions(claude_session_id);

    ALTER TABLE tasks ADD COLUMN thread_key TEXT;
    ALTER TABLE tasks ADD COLUMN last_signal_at TEXT;
    CREATE INDEX idx_tasks_thread_key ON tasks(thread_key);

    CREATE TABLE hook_events (
      id                 INTEGER PRIMARY KEY,
      job_id             TEXT NOT NULL,
      task_id            INTEGER NOT NULL REFERENCES tasks(id),
      claude_session_id  TEXT NOT NULL DEFAULT '',
      prompt_id          TEXT NOT NULL DEFAULT '',
      event              TEXT NOT NULL,      -- 'stop'|'notification'|'session_start'|'session_end'|'heartbeat'
      status             TEXT,               -- for 'stop': COMPLETED|NEEDS_INPUT|FAILED|UNKNOWN
      payload            TEXT NOT NULL,      -- full received JSON (audit, N-01)
      received_at        TEXT NOT NULL,
      UNIQUE (job_id, claude_session_id, prompt_id, event)
    );
    CREATE INDEX idx_hook_events_task ON hook_events(task_id, id);
    "#,
    // v3 — include `status` in the hook_events idempotency key (#131 follow-up,
    // found by real-machine acceptance testing).
    //
    // A Stop-hook `block` makes the agent re-complete WITHIN THE SAME TURN, so the
    // re-completion Stop shares (job_id, claude_session_id, prompt_id, event='stop')
    // with the initial blank Stop but carries a DIFFERENT status
    // (UNKNOWN → COMPLETED). The v2 key dedup'd it as a mere re-delivery and dropped
    // the completion, stranding the task in `dispatched`. Adding `status` lets a
    // status change through while identical re-deliveries (multi-fire / spool
    // re-send / curl retry — same status) still dedup. `status` also becomes
    // NOT NULL DEFAULT '' so the (NULL) status of non-stop events is not treated as
    // distinct under the UNIQUE constraint. SQLite cannot alter a constraint in
    // place, so the table is rebuilt.
    r#"
    ALTER TABLE hook_events RENAME TO hook_events_v2;
    CREATE TABLE hook_events (
      id                 INTEGER PRIMARY KEY,
      job_id             TEXT NOT NULL,
      task_id            INTEGER NOT NULL REFERENCES tasks(id),
      claude_session_id  TEXT NOT NULL DEFAULT '',
      prompt_id          TEXT NOT NULL DEFAULT '',
      event              TEXT NOT NULL,
      status             TEXT NOT NULL DEFAULT '',   -- for 'stop': COMPLETED|NEEDS_INPUT|FAILED|UNKNOWN; '' otherwise
      payload            TEXT NOT NULL,
      received_at        TEXT NOT NULL,
      UNIQUE (job_id, claude_session_id, prompt_id, event, status)
    );
    INSERT INTO hook_events
        (id, job_id, task_id, claude_session_id, prompt_id, event, status, payload, received_at)
      SELECT id, job_id, task_id, claude_session_id, prompt_id, event,
             COALESCE(status, ''), payload, received_at
      FROM hook_events_v2;
    DROP TABLE hook_events_v2;
    CREATE INDEX idx_hook_events_task ON hook_events(task_id, id);
    "#,
    // v4 — generalize the "claude" naming ahead of multi-tool support (#196):
    // the column holds whichever AI tool CLI's native session id (Claude Code
    // today; Codex/OpenCode adapters send the same payload shape), so it is
    // renamed `claude_session_id` → `tool_session_id`. RENAME COLUMN rewrites
    // the column references inside table constraints and index definitions
    // in place (SQLite ≥3.25), so the hook_events UNIQUE idempotency key
    // survives untouched; only the sessions index is recreated for its name.
    r#"
    ALTER TABLE sessions RENAME COLUMN claude_session_id TO tool_session_id;
    ALTER TABLE hook_events RENAME COLUMN claude_session_id TO tool_session_id;
    DROP INDEX idx_sessions_claude_session;
    CREATE INDEX idx_sessions_tool_session ON sessions(tool_session_id);
    "#,
    // v5 — the conversation's message ledger (#242/#257). A task is a
    // *conversation*, so it can receive more than one message; each row is one
    // delivery, and `processed_at IS NULL` is the queue of messages the agent
    // has not been told about yet.
    //
    // Purely additive: nothing reads or writes it until the ingest/dispatch
    // work lands, so an interrupted migration to this version leaves a fully
    // working database (dropping `tasks.thread_key` is deliberately left to a
    // later version for the same reason).
    //
    // Shaped after `hook_events` (v2/v3) because the problem is the same one —
    // idempotently absorbing at-least-once delivery — and that shape is
    // already proven here. `payload` keeps the whole normalized Task verbatim
    // for the audit trail (N-01); the denormalized `author`/`body`/`url`
    // columns exist so reads never have to parse it (this schema has no
    // `json_extract` anywhere, and this is not the place to start).
    //
    // The UNIQUE key is chosen conservatively because SQLite cannot alter a
    // constraint in place — v3 had to rebuild `hook_events` to widen one:
    //
    // - No `revision`/edit timestamp. Including it would turn a typo fix into
    //   an expensive re-run and a second reply; excluding it means an edit
    //   does nothing, which is cheap and obvious. Widening later is the
    //   rebuild; narrowing is not.
    // - No `kind`. What counts as "added to the conversation" is a comment
    //   everywhere (Slack reply, GitHub issue comment); labels and status are
    //   the workflow trigger's concern. A column can still be added later with
    //   plain `ALTER TABLE ADD COLUMN`.
    r#"
    CREATE TABLE task_messages (
      id           INTEGER PRIMARY KEY,
      task_id      INTEGER NOT NULL REFERENCES tasks(id),
      message_key  TEXT NOT NULL,   -- identity of this delivery (Slack: {channel}:{ts}; GitHub: comment id)
      author       TEXT,            -- denormalized for display
      body         TEXT NOT NULL,   -- prompt material
      url          TEXT,            -- permalink
      payload      TEXT NOT NULL,   -- the whole normalized Task as JSON (audit, N-01)
      received_at  TEXT NOT NULL,
      processed_at TEXT,            -- NULL = not yet dispatched; a batch shares one value
      UNIQUE (task_id, message_key)
    );
    CREATE INDEX idx_task_messages_pending ON task_messages(task_id, processed_at);
    "#,
    // v6 — backfill a ledger row for every task that predates v5 (#258).
    //
    // v5 was purely additive, which left existing tasks with an *empty*
    // ledger. That is not a harmless gap: ingest now decides "is this a new
    // message?" from the ledger, so the first re-delivery of any pre-v5 task
    // would look like a brand-new message and reopen a finished task —
    // re-running it and, for a reply-writing source, replying twice.
    // Re-delivery is routine, not exceptional: `plugin_sdk::poll_loop` has no
    // dedup of its own and re-submits everything each tick, relying entirely
    // on the Orchestrator's `duplicate` ack.
    //
    // `message_key = source_task_id` matches what ingest falls back to when a
    // source sends no `message_key`, so those re-deliveries dedup exactly as
    // they did before v5.
    //
    // Backfilled rows are marked processed regardless of the task's state:
    // their instruction is already in `tasks.source_payload`, which is the
    // path dispatch reads today, so they must not be offered as *pending*
    // prompt material. `body` is left empty for the same reason — recovering
    // it would mean parsing `source_payload` in SQL, and this schema
    // deliberately has no JSON traversal anywhere.
    r#"
    INSERT INTO task_messages
        (task_id, message_key, author, body, url, payload, received_at, processed_at)
      SELECT id, source_task_id, NULL, '', url,
             COALESCE(source_payload, '{}'), created_at, updated_at
      FROM tasks
      WHERE id NOT IN (SELECT task_id FROM task_messages);
    "#,
    // v7 — drop `tasks.thread_key` (#242/#264).
    //
    // It correlated a follow-up message's *new* task with the prior task of
    // the same Slack thread. #242 removed the premise: a follow-up is another
    // message of the *same* task, so there is nothing left to correlate and
    // the only production reader (`thread_resume_session_id`) is gone.
    //
    // Deliberately its own version, after the ingest and dispatch work
    // landed: dropping it in v5 would have broken resume for every version
    // between there and #259.
    //
    // A dead column is not free — it reads as a supported field, and a source
    // plugin setting it would silently get nothing.
    //
    // `DROP COLUMN` needs SQLite ≥3.35; `rusqlite`'s bundled build is well
    // past that, and this project only ever talks to its own bundled copy.
    r#"
    DROP INDEX idx_tasks_thread_key;
    ALTER TABLE tasks DROP COLUMN thread_key;
    "#,
    // v8 — record the commit a task's worktree was branched from.
    //
    // Cleanup deletes a task's branch once every commit on it is also on
    // `origin`. That test says nothing about *whose* branch it is, which was
    // fine only because the name was orchestrator-generated and therefore
    // could not collide with anything a human made. Once the agent picks the
    // name from the repository's own convention, the name lands in the same
    // namespace the operator uses, and "fully pushed" describes plenty of
    // branches cleanup has no business deleting.
    //
    // The base commit is what distinguishes them: a branch cut from an older
    // default branch does not contain this task's starting point. It is
    // already computed during creation and was simply discarded.
    //
    // Nullable, and rows written before this version stay NULL — cleanup
    // treats an absent base commit as "cannot prove ownership" and keeps the
    // branch, matching how it already handles an uncountable commit count.
    r#"
    ALTER TABLE tasks ADD COLUMN base_commit TEXT;
    "#,
    // v9 — a version number for optimistic concurrency on state (#763).
    //
    // Two processes write task state: the engine, and `totsuka task cancel` /
    // `retry` writing the DB directly when no engine answers (#760). The
    // engine reads a task, awaits an agent for seconds, then writes a
    // transition decided against what it read. Comparing the *state* at write
    // time is not enough — cancel → retry → re-dispatch returns the task to
    // the very state the engine read (ABA), and the stale write would land on
    // the new attempt. A counter bumped by every transition is.
    //
    // Existing rows start at 0; only transitions bump it, never notes.
    r#"
    ALTER TABLE tasks ADD COLUMN state_version INTEGER NOT NULL DEFAULT 0;
    "#,
];

/// Add `schema_migrations.applied_by` to a ledger created before the column
/// existed (#275).
///
/// Nullable on purpose: rows written by binaries that predate the column stay
/// NULL, which reads as "unknown" rather than being backfilled with a version
/// that did not actually apply them.
///
/// Called from bootstrap, not from `MIGRATIONS` — see the comment at the call
/// site for why the ledger table cannot be versioned by its own ledger.
fn ensure_applied_by_column(conn: &Connection) -> Result<(), StateError> {
    if !has_applied_by_column(conn)? {
        conn.execute_batch("ALTER TABLE schema_migrations ADD COLUMN applied_by TEXT;")?;
    }
    Ok(())
}

/// The totsuka version that applied schema `version`, if the ledger records
/// one (#275).
///
/// Returns `None` both when no such row exists and when the row's
/// `applied_by` is NULL — and, crucially, when the ledger has no `applied_by`
/// column at all. That last case is why this tolerates a missing column
/// rather than propagating: it runs inside the "DB is too new" error path,
/// where failing to read a *diagnostic* must not replace an actionable
/// message with `no such column`.
fn applied_by_of(conn: &Connection, version: i64) -> Result<Option<String>, StateError> {
    if !has_applied_by_column(conn)? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT applied_by FROM schema_migrations WHERE version = ?1",
            params![version],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Highest applied schema version, or 0 when the ledger table does not exist
/// yet (#275) — the non-migrating open never creates it.
fn current_schema_version(conn: &Connection) -> Result<i64, StateError> {
    let has_ledger = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_ledger {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?)
}

/// Whether `schema_migrations` already has the `applied_by` column (#275).
fn has_applied_by_column(conn: &Connection) -> Result<bool, StateError> {
    let mut stmt = conn.prepare("PRAGMA table_info(schema_migrations)")?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(names.iter().any(|n| n == "applied_by"))
}

impl StateDb {
    /// Open (creating if needed) a file-backed state DB and run migrations.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        Self::open_with_clock(path, Arc::new(SystemClock))
    }

    /// [`open`](Self::open) with an injected [`Clock`] (#174) — the seam
    /// deterministic tests use to control every persisted timestamp.
    pub fn open_with_clock(path: &Path, clock: Arc<dyn Clock>) -> Result<Self, StateError> {
        let preexisting = path.exists();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some((path.to_path_buf(), preexisting)), clock, true)
    }

    /// Open a file-backed state DB **without** applying migrations (#275).
    ///
    /// Every command that does not hold `run.lock` goes through here, so
    /// schema changes only ever happen under that lock — which only
    /// `totsuka run` takes. Note the criterion is the lock, not read vs
    /// write: `task cancel` / `retry` mutate rows through this entry point
    /// and still must not migrate. Before it existed, `status` and `run`
    /// racing right after an upgrade could both start migrating the same
    /// file with no lock between them.
    ///
    /// Makes no schema or ledger write of its own — not even the
    /// `applied_by` bootstrap ALTER, and it never creates the file. (SQLite
    /// may still checkpoint the WAL when the last connection closes, as it
    /// does for any connection; that folds already-committed pages in and
    /// changes no logical content.)
    ///
    /// Fails with [`StateError::SchemaOutdated`] if migrations are pending,
    /// and (like [`open`](Self::open)) with [`StateError::SchemaTooNew`] if
    /// the DB is from a newer totsuka.
    pub fn open_no_migrate(path: &Path) -> Result<Self, StateError> {
        Self::open_no_migrate_with_clock(path, Arc::new(SystemClock))
    }

    /// [`open_no_migrate`](Self::open_no_migrate) with an injected [`Clock`]
    /// (#174).
    pub fn open_no_migrate_with_clock(
        path: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StateError> {
        // Deliberately not `Connection::open`: its default flags include
        // `CREATE`, which would turn "no state.db yet" into a silently
        // created empty one. Read-write (the caller may still `task cancel`),
        // just never conjuring the file.
        //
        // No `journal_mode` pragma either — WAL is persistent in the file, so
        // a DB totsuka created already has it, and issuing the pragma on a
        // non-WAL file would be a write on a path that promises none.
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::init(conn, None, clock, false)
    }

    /// Open an ephemeral in-memory DB (tests).
    pub fn open_in_memory() -> Result<Self, StateError> {
        Self::open_in_memory_with_clock(Arc::new(SystemClock))
    }

    /// [`open_in_memory`](Self::open_in_memory) with an injected [`Clock`]
    /// (#174).
    pub fn open_in_memory_with_clock(clock: Arc<dyn Clock>) -> Result<Self, StateError> {
        Self::init(Connection::open_in_memory()?, None, clock, true)
    }

    /// The DB's schema version and the totsuka version that applied it
    /// (#275). `None` for the second element means the ledger predates
    /// `applied_by` — "unknown", not "this binary".
    pub fn schema_version(&self) -> Result<(i64, Option<String>), StateError> {
        let version: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?;
        Ok((version, applied_by_of(&self.conn, version)?))
    }

    /// Shared init: enable FKs, check schema compatibility, and — when
    /// `allow_migrate` — run pending migrations (with backup first).
    ///
    /// `allow_migrate` is false for [`open_no_migrate`](Self::open_no_migrate),
    /// which must not write to the DB at all.
    fn init(
        mut conn: Connection,
        backup: Option<(PathBuf, bool)>,
        clock: Arc<dyn Clock>,
        allow_migrate: bool,
    ) -> Result<Self, StateError> {
        // rusqlite defaults foreign_keys OFF; the schema declares FKs.
        // Connection-scoped, so this is not a file write.
        conn.pragma_update(None, "foreign_keys", "ON")?;

        // Read the version and settle compatibility *before* touching
        // anything. Both reads tolerate a ledger that does not exist yet
        // (version 0) or predates `applied_by` (no attribution), so nothing
        // has to be created first — which is what lets a DB we are about to
        // refuse stay completely untouched.
        let current = current_schema_version(&conn)?;
        // Compatibility is judged on the *schema* version, never the app
        // version: a patch release that changes no schema must not refuse a
        // DB written by its neighbour.
        let supported = MIGRATIONS.len() as i64;
        if current > supported {
            // The version one past what we support is the first one we cannot
            // apply, so whoever introduced *it* is the release the operator
            // needs. The ledger may predate `applied_by`, hence the Option.
            let introduced_by = match applied_by_of(&conn, supported + 1)? {
                Some(v) => format!("。v{} を導入したのは {v} です", supported + 1),
                None => String::new(),
            };
            return Err(StateError::SchemaTooNew {
                found: current,
                supported,
                app: env!("CARGO_PKG_VERSION").to_string(),
                introduced_by,
            });
        }
        if !allow_migrate && current < supported {
            return Err(StateError::SchemaOutdated {
                found: current,
                expected: supported,
            });
        }

        if allow_migrate {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version    INTEGER PRIMARY KEY,
                    applied_at TEXT NOT NULL,
                    applied_by TEXT
                );",
            )?;
            // Widen a ledger created before `applied_by` existed. This has to
            // happen here, in bootstrap, *before* the apply loop — never as a
            // `MIGRATIONS` entry. `schema_migrations` is the table that
            // versions those entries, so an ALTER expressed as version N
            // would run after the INSERT of every version below N, and those
            // INSERTs write the column: upgrading a v5 DB straight to v8
            // would fail with `no such column: applied_by`. Bootstrapping it
            // breaks the cycle.
            ensure_applied_by_column(&conn)?;
        }

        if (current as usize) < MIGRATIONS.len() {
            // Back up the DB file before mutating its schema (§10.3).
            let mut backup_path = None;
            if let Some((path, true)) = &backup {
                // Flush any WAL into the main db first; in WAL mode a plain
                // file copy would otherwise miss uncheckpointed pages and
                // produce an unrestorable backup.
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
                // The pre-migration schema version is part of the name: a
                // single fixed `.bak` is overwritten on every upgrade, so a
                // run that spans two versions leaves no way back to the
                // intermediate one, and a `.bak` sitting on disk says nothing
                // about which schema it holds.
                let bak = PathBuf::from(format!("{}.v{current}.bak", path.display()));
                fs::copy(path, &bak)?;
                backup_path = Some(bak);
            }
            tracing::info!(
                from = current,
                to = MIGRATIONS.len() as i64,
                backup = backup_path
                    .as_ref()
                    .map_or_else(|| "none".to_string(), |p| p.display().to_string()),
                "applying state.db migrations"
            );
            for (i, sql) in MIGRATIONS.iter().enumerate() {
                let version = (i + 1) as i64;
                if version > current {
                    let tx = conn.transaction()?;
                    tx.execute_batch(sql)?;
                    tx.execute(
                        "INSERT INTO schema_migrations (version, applied_at, applied_by) \
                         VALUES (?1, ?2, ?3)",
                        params![version, clock.now_rfc3339(), env!("CARGO_PKG_VERSION")],
                    )?;
                    tx.commit()?;
                }
            }
        }
        Ok(Self { conn, clock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::state_db::test_support::*;
    use crate::adapters::state_db::{HookEventInsert, HookEventOutcome, TaskMessageOutcome};
    use crate::domain::state::{TaskEvent, TaskState};
    use crate::domain::task::{SourceTaskId, TaskId};

    /// Real-clock RFC 3339 timestamp for direct-INSERT helpers whose exact
    /// value is irrelevant.
    fn now() -> String {
        SystemClock.now_rfc3339()
    }

    #[test]
    fn migrates_v1_to_v2_backing_up_first() {
        // A pre-existing v1 DB must be backed up, then migrated to v2 in place,
        // preserving its rows (§10.3, `survives_reopen_from_disk` style).
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v2", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        // Build a v1-only database by hand (schema_migrations pinned at 1).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1)",
                params![now()],
            )
            .unwrap();
            // Seed a task so we can prove data survives the schema change.
            conn.execute(
                "INSERT INTO tasks
                    (source, source_task_id, workflow, mode, state, priority,
                     title, created_at, updated_at)
                 VALUES ('github','7','implement','implement','queued',0,'legacy',?1,?1)",
                params![now()],
            )
            .unwrap();
        }

        // Reopen through StateDb: v2 applies and the old file is backed up.
        let db = StateDb::open(&path).unwrap();
        let bak = PathBuf::from(format!("{}.v1.bak", path.display()));
        assert!(
            bak.exists(),
            "existing DB backed up before migrating, named for the schema \
             version it holds (§10.3, #275)"
        );

        // The v1 row survived; the new columns read back as NULL on it.
        let task = db
            .find_by_source("github", &SourceTaskId("7".into()))
            .unwrap()
            .unwrap();
        assert_eq!(task.title, "legacy");
        assert_eq!(task.last_signal_at, None);

        // v2 objects now exist: a hook event and a session column round-trip.
        assert_eq!(
            db.record_hook_event(&hook_event(task.id, "job-7-1", "stop", Some("COMPLETED")))
                .unwrap(),
            HookEventOutcome::New
        );
        let sess = db.record_session(task.id, "herdr", "sess-1").unwrap();
        db.set_tool_session_id(sess, "cc-1").unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_v2_to_v3_preserving_hook_events() {
        // A pre-existing v2 DB (4-col hook_events key, nullable status) must
        // migrate to v3 in place: rows preserved (ids kept), a NULL status
        // normalised to '', and the new 5-col key active so a block re-completion
        // (UNKNOWN → COMPLETED, same key) records instead of being deduped.
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v3", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        // Build a v2 database by hand (schema_migrations pinned at 2).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch(MIGRATIONS[1]).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?1), (2, ?1)",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks
                    (source, source_task_id, workflow, mode, state, priority,
                     title, created_at, updated_at)
                 VALUES ('slack','C:1','slack-reply','plan','dispatched',0,'legacy',?1,?1)",
                params![now()],
            )
            .unwrap();
            // A stop (status set) and a non-stop event whose status is NULL — v2
            // allowed NULL for non-stop events. The column still bears its
            // pre-v4 name `claude_session_id` at this schema version.
            conn.execute(
                "INSERT INTO hook_events
                    (id, job_id, task_id, claude_session_id, prompt_id, event, status,
                     payload, received_at)
                 VALUES (1,'job-1-1',1,'s','p','stop','UNKNOWN','{}',?1),
                        (2,'job-1-1',1,'s','','session_start',NULL,'{}',?1)",
                params![now()],
            )
            .unwrap();
        }

        // Reopen through StateDb: v3 applies and the old file is backed up.
        let db = StateDb::open(&path).unwrap();
        assert!(
            PathBuf::from(format!("{}.v2.bak", path.display())).exists(),
            "existing DB backed up before migrating (§10.3)"
        );

        // Both v2 rows survive the rebuild; the NULL status is normalised to ''.
        let (n, blanks): (i64, i64) = db
            .conn
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN status = '' THEN 1 ELSE 0 END) FROM hook_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 2, "both v2 rows survive the rebuild");
        assert_eq!(blanks, 1, "the NULL status is normalised to ''");
        let unknown_status: String = db
            .conn
            .query_row("SELECT status FROM hook_events WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(unknown_status, "UNKNOWN", "the stop status is preserved");

        // The v3 5-col key is active: a COMPLETED re-completion sharing the seeded
        // UNKNOWN stop's (job, session, prompt) is a NEW row, not a Duplicate.
        let done = HookEventInsert {
            job_id: "job-1-1".into(),
            task_id: TaskId(1),
            tool_session_id: "s".into(),
            prompt_id: "p".into(),
            event: "stop".into(),
            status: Some("COMPLETED".into()),
            payload: "{}".into(),
        };
        assert_eq!(db.record_hook_event(&done).unwrap(), HookEventOutcome::New);
        assert_eq!(
            db.record_hook_event(&done).unwrap(),
            HookEventOutcome::Duplicate
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_v3_to_v4_renaming_session_columns() {
        // A pre-existing v3 DB must migrate to v4 in place: the
        // `claude_session_id` columns come back as `tool_session_id` with data
        // intact, and the rebuilt-by-rename UNIQUE idempotency key still
        // dedups (#196 rename).
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v4", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        // Build a v3 database by hand (schema_migrations pinned at 3).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch(MIGRATIONS[1]).unwrap();
            conn.execute_batch(MIGRATIONS[2]).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) \
                 VALUES (1, ?1), (2, ?1), (3, ?1)",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks
                    (source, source_task_id, workflow, mode, state, priority,
                     title, created_at, updated_at)
                 VALUES ('github','9','implement','implement','dispatched',0,'legacy',?1,?1)",
                params![now()],
            )
            .unwrap();
            // A session and a hook event under the pre-v4 column name.
            conn.execute(
                "INSERT INTO sessions (task_id, plugin, session_id, created_at, claude_session_id)
                 VALUES (1, 'herdr', 'sess-1', ?1, 'cc-old')",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO hook_events
                    (id, job_id, task_id, claude_session_id, prompt_id, event, status,
                     payload, received_at)
                 VALUES (1,'job-1-1',1,'cc-old','p','stop','COMPLETED','{}',?1)",
                params![now()],
            )
            .unwrap();
        }

        // Reopen through StateDb: v4 applies and the old file is backed up.
        let db = StateDb::open(&path).unwrap();
        assert!(
            PathBuf::from(format!("{}.v3.bak", path.display())).exists(),
            "existing DB backed up before migrating (§10.3)"
        );

        // The session row reads back through the renamed column.
        let rec = db.latest_session(TaskId(1)).unwrap().unwrap();
        assert_eq!(rec.tool_session_id.as_deref(), Some("cc-old"));
        assert_eq!(
            db.find_session_by_tool_session_id("cc-old")
                .unwrap()
                .unwrap()
                .id,
            rec.id
        );

        // The idempotency key survived the rename: a same-key re-delivery
        // still dedups, a different status still records.
        let redelivery = HookEventInsert {
            job_id: "job-1-1".into(),
            task_id: TaskId(1),
            tool_session_id: "cc-old".into(),
            prompt_id: "p".into(),
            event: "stop".into(),
            status: Some("COMPLETED".into()),
            payload: "{}".into(),
        };
        assert_eq!(
            db.record_hook_event(&redelivery).unwrap(),
            HookEventOutcome::Duplicate
        );
        let changed = HookEventInsert {
            status: Some("NEEDS_INPUT".into()),
            ..redelivery
        };
        assert_eq!(
            db.record_hook_event(&changed).unwrap(),
            HookEventOutcome::New
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_v4_to_v5_adding_task_messages_without_touching_anything_else() {
        // v5 is purely additive (#257): an existing database gains the
        // `task_messages` table and keeps every row it already had, so an
        // upgrade that stops here still runs the old code paths correctly.
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v5", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        // Build a v4 database by hand (schema_migrations pinned at 4).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for m in &MIGRATIONS[0..4] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) \
                 VALUES (1, ?1), (2, ?1), (3, ?1), (4, ?1)",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks
                    (source, source_task_id, workflow, mode, state, priority,
                     title, created_at, updated_at)
                 VALUES ('github','9','implement','implement','done',0,'legacy',?1,?1)",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (task_id, plugin, session_id, created_at, tool_session_id)
                 VALUES (1, 'herdr', 'sess-1', ?1, 'cc-1')",
                params![now()],
            )
            .unwrap();
        }

        let db = StateDb::open(&path).unwrap();
        assert!(
            PathBuf::from(format!("{}.v4.bak", path.display())).exists(),
            "existing DB backed up before migrating (§10.3)"
        );

        // Pre-existing rows are untouched.
        let rec = db.get_task(TaskId(1)).unwrap().unwrap();
        assert_eq!(rec.title, "legacy");
        assert_eq!(rec.state, TaskState::Done);
        assert_eq!(
            db.latest_session(TaskId(1))
                .unwrap()
                .unwrap()
                .tool_session_id,
            Some("cc-1".to_string())
        );

        // The ledger is usable, and v6 has given this pre-existing task its
        // backfilled row (see `migrates_v5_to_v6_...` for why that matters).
        assert_eq!(keys(&db.list_task_messages(TaskId(1)).unwrap()), ["9"]);
        assert!(db.pending_task_messages(TaskId(1)).unwrap().is_empty());
        assert_eq!(
            db.append_task_message(&message(TaskId(1), "m1", "hello"))
                .unwrap(),
            TaskMessageOutcome::New
        );
        assert_eq!(
            keys(&db.list_task_messages(TaskId(1)).unwrap()),
            ["9", "m1"]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Tasks that predate the ledger must come out of the migration with one
    /// already-processed row each (#258). Without it, ingest would read their
    /// empty ledger as "never seen a message" and reopen finished tasks on the
    /// first re-delivery — and `poll_loop` re-delivers everything every tick.
    #[test]
    fn migrates_v5_to_v6_backfilling_a_ledger_row_per_existing_task() {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v6", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for m in &MIGRATIONS[0..5] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) \
                 VALUES (1, ?1), (2, ?1), (3, ?1), (4, ?1), (5, ?1)",
                params![now()],
            )
            .unwrap();
            // A finished task and a still-queued one, both with empty ledgers.
            conn.execute(
                "INSERT INTO tasks
                    (id, source, source_task_id, workflow, mode, state, priority,
                     title, url, source_payload, created_at, updated_at)
                 VALUES (1,'github','9','implement','implement','done',0,'done one',
                         'https://example.com/9', '{\"id\":\"9\"}', ?1, ?1)",
                params![now()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks
                    (id, source, source_task_id, workflow, mode, state, priority,
                     title, created_at, updated_at)
                 VALUES (2,'github','10','implement','implement','queued',0,'queued one',?1,?1)",
                params![now()],
            )
            .unwrap();
        }

        let db = StateDb::open(&path).unwrap();

        for (task_id, key) in [(1, "9"), (2, "10")] {
            let ledger = db.list_task_messages(TaskId(task_id)).unwrap();
            assert_eq!(keys(&ledger), [key], "one row per pre-existing task");
            assert!(
                ledger[0].processed_at.is_some(),
                "backfilled rows must not look like queued prompt material"
            );
            assert!(
                db.pending_task_messages(TaskId(task_id))
                    .unwrap()
                    .is_empty()
            );
            // ...so the source's next re-delivery dedups instead of reopening.
            assert_eq!(
                db.append_task_message_reopening(
                    &message(TaskId(task_id), key, "re-delivered"),
                    None
                )
                .unwrap(),
                (TaskMessageOutcome::Duplicate, None)
            );
        }
        assert_eq!(
            db.get_task(TaskId(1)).unwrap().unwrap().state,
            TaskState::Done
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_v6_to_v7_dropping_thread_key_without_touching_the_rows() {
        // The column is dead (#242 made `Task.id` the conversation), but the
        // tasks that carried it are not: dropping it must leave every row and
        // every other column exactly as they were.
        let dir = std::env::temp_dir().join(format!("totsuka-{}-migrate_v7", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for m in &MIGRATIONS[0..6] {
                conn.execute_batch(m).unwrap();
            }
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) \
                 VALUES (1, ?1), (2, ?1), (3, ?1), (4, ?1), (5, ?1), (6, ?1)",
                params![now()],
            )
            .unwrap();
            // A Slack task from the thread_key era, with the column populated.
            conn.execute(
                "INSERT INTO tasks
                    (id, source, source_task_id, workflow, mode, state, priority,
                     title, url, created_at, updated_at, thread_key, last_signal_at)
                 VALUES (1,'slack','C1:100.1','slack-reply','plan','done',3,'legacy',
                         'https://slack.test/1', ?1, ?1, 'C1:100.0', ?1)",
                params![now()],
            )
            .unwrap();
        }

        let db = StateDb::open(&path).unwrap();

        let task = db
            .find_by_source("slack", &SourceTaskId("C1:100.1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(task.title, "legacy");
        assert_eq!(task.state, TaskState::Done);
        assert_eq!(task.priority, 3);
        assert_eq!(task.url.as_deref(), Some("https://slack.test/1"));
        // Its neighbour in the same v2 migration must not have gone with it.
        assert!(task.last_signal_at.is_some());

        // The column is really gone, not merely unread — a stale query against
        // it must fail rather than quietly keep working.
        let conn = Connection::open(&path).unwrap();
        assert!(
            conn.query_row("SELECT thread_key FROM tasks", [], |_| Ok(()))
                .is_err(),
            "thread_key must no longer exist as a column"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Every row this binary writes carries the version that wrote it (#275),
    /// so "which totsuka introduced schema vN" is answerable after the fact.
    #[test]
    fn records_applied_by_for_newly_applied_migrations() {
        let db = StateDb::open_in_memory().unwrap();
        let rows: Vec<(i64, Option<String>)> = db
            .conn
            .prepare("SELECT version, applied_by FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(rows.len(), MIGRATIONS.len(), "every version is recorded");
        for (version, applied_by) in rows {
            assert_eq!(
                applied_by.as_deref(),
                Some(env!("CARGO_PKG_VERSION")),
                "v{version} was applied by this binary"
            );
        }
    }

    /// A ledger from before the column existed must be widened in place, not
    /// rejected, and its pre-existing rows must stay NULL — this binary did
    /// not apply them and must not claim it did.
    #[test]
    fn adds_applied_by_to_a_legacy_ledger_lacking_the_column() {
        let dir =
            std::env::temp_dir().join(format!("totsuka-{}-applied_by_alter", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        // Fully migrated, but with the two-column ledger older binaries wrote:
        // nothing to apply, so only the bootstrap ALTER can add the column.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for (i, m) in MIGRATIONS.iter().enumerate() {
                conn.execute_batch(m).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![(i + 1) as i64, now()],
                )
                .unwrap();
            }
        }

        let db = StateDb::open(&path).unwrap();
        let unknown: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE applied_by IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            unknown,
            MIGRATIONS.len() as i64,
            "pre-existing rows read as unknown, not as applied by this binary"
        );
        assert!(
            !PathBuf::from(format!("{}.v{}.bak", path.display(), MIGRATIONS.len())).exists(),
            "an up-to-date DB has nothing to migrate, so nothing to back up"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression for the ordering trap that keeps `applied_by` out of
    /// `MIGRATIONS` (#275): applying more than one version in a single open
    /// must not hit `no such column: applied_by`. Were the ALTER expressed as
    /// a migration, the older version's INSERT would run before it.
    #[test]
    fn applies_two_versions_at_once_over_a_legacy_ledger() {
        let last = MIGRATIONS.len();
        assert!(last >= 2, "the trap needs at least two versions to span");
        let behind = last - 2;

        let dir = std::env::temp_dir().join(format!("totsuka-{}-two_versions", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for (i, m) in MIGRATIONS[..behind].iter().enumerate() {
                conn.execute_batch(m).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![(i + 1) as i64, now()],
                )
                .unwrap();
            }
        }

        let db = StateDb::open(&path).unwrap();

        // The backup is named for the schema it holds — the version we came
        // *from*, so a rollback can pick the right generation.
        assert!(
            PathBuf::from(format!("{}.v{behind}.bak", path.display())).exists(),
            "backup names the pre-migration version"
        );

        let stamped: Vec<(i64, Option<String>)> = db
            .conn
            .prepare("SELECT version, applied_by FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(stamped.len(), last);
        for (version, applied_by) in stamped {
            let expected = if version as usize > behind {
                Some(env!("CARGO_PKG_VERSION"))
            } else {
                None
            };
            assert_eq!(
                applied_by.as_deref(),
                expected,
                "v{version}: only the versions this open applied are stamped"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    /// Build a fully-migrated DB at `path` and return its dir, for the guard
    /// tests below to then tamper with.
    fn migrated_db_dir(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        StateDb::open(&path).unwrap();
        (dir, path)
    }

    /// A DB from a newer totsuka must stop *both* entry points (#275). The
    /// migrating path would otherwise skip its `current < len` branch and
    /// return Ok, running happily against a schema it does not know.
    #[test]
    fn refuses_a_db_newer_than_the_binary() {
        let (dir, path) = migrated_db_dir("too_new");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at, applied_by) \
                 VALUES (?1, ?2, '9.9.9')",
                params![MIGRATIONS.len() as i64 + 1, now()],
            )
            .unwrap();
        }

        for (label, err) in [
            (
                "open",
                StateDb::open(&path).err().expect("open must refuse"),
            ),
            (
                "open_no_migrate",
                StateDb::open_no_migrate(&path)
                    .err()
                    .expect("open_no_migrate must refuse"),
            ),
        ] {
            assert!(
                matches!(err, StateError::SchemaTooNew { .. }),
                "{label} must refuse a newer schema, got {err:?}"
            );
            let msg = err.to_string();
            // The whole point is telling the operator where to go.
            assert!(
                msg.contains("9.9.9"),
                "{label} message must name the release that introduced the \
                 unknown version, got: {msg}"
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    /// The too-new guard reads `applied_by` for its hint. On a ledger old
    /// enough to lack the column, that read must degrade to "no hint" — never
    /// replace the actionable error with `no such column`.
    #[test]
    fn too_new_guard_survives_a_ledger_without_applied_by() {
        let (dir, path) = migrated_db_dir("too_new_legacy");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("ALTER TABLE schema_migrations DROP COLUMN applied_by;")
                .unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                params![MIGRATIONS.len() as i64 + 1, now()],
            )
            .unwrap();
        }

        let err = StateDb::open_no_migrate(&path)
            .err()
            .expect("a newer schema must be refused");
        assert!(matches!(err, StateError::SchemaTooNew { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("v8") || msg.contains(&format!("v{}", MIGRATIONS.len() + 1)));
        assert!(
            !msg.contains("no such column"),
            "a missing diagnostic column must not leak as the error: {msg}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// A DB behind the binary is an error on the non-migrating path — that is
    /// what sends the operator to `totsuka run` instead of letting a
    /// lock-less command migrate.
    #[test]
    fn open_no_migrate_refuses_an_outdated_db() {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-outdated", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for (i, m) in MIGRATIONS[..MIGRATIONS.len() - 1].iter().enumerate() {
                conn.execute_batch(m).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![(i + 1) as i64, now()],
                )
                .unwrap();
            }
        }

        let err = StateDb::open_no_migrate(&path)
            .err()
            .expect("a pending migration must be refused");
        assert!(matches!(err, StateError::SchemaOutdated { .. }), "{err:?}");
        assert!(err.to_string().contains("totsuka run"), "{err}");

        // …while the migrating path still upgrades it.
        StateDb::open(&path).unwrap();
        assert_eq!(
            StateDb::open_no_migrate(&path)
                .unwrap()
                .schema_version()
                .unwrap()
                .0,
            MIGRATIONS.len() as i64
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// v9 (#763): a task that existed before the version column starts at
    /// 0, and can be moved through a reference taken after the upgrade.
    #[test]
    fn tasks_from_before_v9_start_at_version_zero() {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-pre_v9", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations \
                 (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for (i, m) in MIGRATIONS[..8].iter().enumerate() {
                conn.execute_batch(m).unwrap();
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![(i + 1) as i64, now()],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO tasks (source, source_task_id, workflow, mode, state, title, \
                 created_at, updated_at) VALUES ('github', '1', 'wf', 'implement', 'queued', \
                 't', ?1, ?1)",
                params![now()],
            )
            .unwrap();
        }

        let db = StateDb::open(&path).unwrap();
        let task = db
            .find_by_source("github", &SourceTaskId("1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(task.state_version, 0);
        let (state, moved) = db
            .apply_event(task.task_ref(), TaskEvent::Dispatch, None)
            .unwrap();
        assert_eq!((state, moved.version()), (TaskState::Dispatched, 1));
        drop(db);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The non-migrating open must not touch the file — no ledger rows, no
    /// bootstrap ALTER, no backup. This is the property that makes it safe to
    /// run outside `run.lock`.
    #[test]
    fn open_no_migrate_does_not_write() {
        let (dir, path) = migrated_db_dir("no_write");
        // Drop the column so a stray `ensure_applied_by_column` would show up.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE); \
                 ALTER TABLE schema_migrations DROP COLUMN applied_by;",
            )
            .unwrap();
        }
        let before = fs::read(&path).unwrap();

        let db = StateDb::open_no_migrate(&path).unwrap();
        let (version, applied_by) = db.schema_version().unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert_eq!(applied_by, None, "no column, so no attribution");
        drop(db);

        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "open_no_migrate must leave the file byte-identical"
        );
        assert!(!has_applied_by_column(&Connection::open(&path).unwrap()).unwrap());
        assert!(
            !dir.join("state.db.v7.bak").exists()
                && !PathBuf::from(format!("{}.v{}.bak", path.display(), MIGRATIONS.len())).exists(),
            "no backup from a non-migrating open"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// A missing file must not become a silently created empty DB — the
    /// non-migrating path drops SQLite's `CREATE` flag for exactly this.
    #[test]
    fn open_no_migrate_does_not_create_the_file() {
        let dir = std::env::temp_dir().join(format!("totsuka-{}-no_create", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        assert!(StateDb::open_no_migrate(&path).is_err());
        assert!(!path.exists(), "no state.db conjured out of nothing");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A DB we refuse must come out untouched — including on the *migrating*
    /// path, where the compatibility verdict is settled before the ledger
    /// bootstrap so the ALTER never lands on a database we then reject.
    #[test]
    fn a_refused_db_is_left_untouched_by_the_migrating_open() {
        let (dir, path) = migrated_db_dir("too_new_untouched");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at, applied_by) \
                 VALUES (?1, ?2, '9.9.9')",
                params![MIGRATIONS.len() as i64 + 1, now()],
            )
            .unwrap();
            // Strip the column so a premature bootstrap ALTER would be visible.
            conn.execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE); \
                 ALTER TABLE schema_migrations DROP COLUMN applied_by;",
            )
            .unwrap();
        }
        let before = fs::read(&path).unwrap();

        assert!(StateDb::open(&path).is_err());

        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "refusing a too-new DB must not write to it"
        );
        assert!(
            !has_applied_by_column(&Connection::open(&path).unwrap()).unwrap(),
            "the bootstrap ALTER must not run on a DB we reject"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// `schema_version` is what `doctor` prints.
    #[test]
    fn schema_version_reports_the_applying_release() {
        let db = StateDb::open_in_memory().unwrap();
        let (version, applied_by) = db.schema_version().unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert_eq!(applied_by.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }
}
