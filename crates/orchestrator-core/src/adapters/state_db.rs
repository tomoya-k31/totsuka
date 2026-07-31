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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use crate::adapters::clock::SystemClock;
use crate::domain::state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};
use crate::ports::clock::Clock;

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
];

/// `events.detail` for the ingest event. Stored as JSON so consumers can
/// parse every `detail` value uniformly.
const INGEST_DETAIL: &str = r#"{"kind":"ingested"}"#;

/// `events.detail` for a push ingest (`task/submit`, 0.1.6).
const SUBMIT_DETAIL: &str = r#"{"kind":"submitted"}"#;

/// Columns of `tasks`, read by name in [`row_to_task`].
const TASK_COLUMNS: &str = "id, source, source_task_id, workflow, mode, repo, \
     worktree_path, branch, base_commit, state, priority, title, url, source_payload, \
     finished_at, created_at, updated_at, last_signal_at";

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
    /// JSON (de)serialization of `source_payload`/`detail` failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// A stored state string was not recognized.
    #[error(transparent)]
    UnknownState(#[from] UnknownState),
    /// No task with the given id.
    #[error("task not found: {0}")]
    NotFound(i64),
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

/// A task to ingest (F-01). Starts life in [`TaskState::Queued`].
#[derive(Debug, Clone)]
pub struct NewTask {
    /// Source plugin instance name (e.g. `github`).
    pub source: String,
    /// The source's own task id (Issue number, Notion page id).
    pub source_task_id: String,
    /// Matched workflow name.
    pub workflow: String,
    /// Execution mode copied from the workflow (`plan`/`implement`).
    pub mode: String,
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
    pub last_signal_at: Option<String>,
}

/// A persisted task row.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    /// Row id.
    pub id: i64,
    /// Source plugin instance name.
    pub source: String,
    /// Source's own task id.
    pub source_task_id: String,
    /// Matched workflow name.
    pub workflow: String,
    /// Execution mode.
    pub mode: String,
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
    pub finished_at: Option<String>,
    /// Ingest timestamp (ISO 8601 UTC).
    pub created_at: String,
    /// Last-update timestamp (ISO 8601 UTC).
    pub updated_at: String,
    /// Timestamp of the last hook signal (R-10 timeout anchor; ISO 8601 UTC).
    pub last_signal_at: Option<String>,
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
    pub task_id: i64,
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

/// A persisted audit event (F-72), for `task show` history.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    /// Row id.
    pub id: i64,
    /// Owning task id.
    pub task_id: i64,
    /// State before the transition (`None` for the ingest event).
    pub from_state: Option<TaskState>,
    /// State after the transition.
    pub to_state: TaskState,
    /// Timestamp (ISO 8601 UTC).
    pub occurred_at: String,
    /// Structured detail, if recorded.
    pub detail: Option<serde_json::Value>,
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
    pub task_id: i64,
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
    pub task_id: i64,
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
    pub task_id: i64,
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

/// Columns of `task_messages`, read by name in [`row_to_task_message`].
const TASK_MESSAGE_COLUMNS: &str = "id, task_id, message_key, author, body, url, \
     payload, received_at, processed_at";

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

/// The SQLite state database.
pub struct StateDb {
    conn: Connection,
    clock: Arc<dyn Clock>,
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

    /// Ingest a task idempotently (F-73). Returns its id, whether newly
    /// inserted or already present under the same `(source, source_task_id)`.
    pub fn upsert_task(&self, task: &NewTask) -> Result<i64, StateError> {
        self.upsert_task_inner(task, INGEST_DETAIL)
    }

    /// [`upsert_task`](Self::upsert_task) for the push path (`task/submit`,
    /// 0.1.6): identical semantics, but the ingest audit event records
    /// `{"kind":"submitted"}` so push and fetch ingests stay distinguishable.
    pub fn upsert_submitted_task(&self, task: &NewTask) -> Result<i64, StateError> {
        self.upsert_task_inner(task, SUBMIT_DETAIL)
    }

    fn upsert_task_inner(&self, task: &NewTask, detail: &str) -> Result<i64, StateError> {
        let now = self.clock.now_rfc3339();
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
                task.mode,
                task.repo,
                TaskState::Queued.as_str(),
                task.priority,
                task.title,
                task.url,
                payload,
                now,
                task.last_signal_at,
            ],
        )?;
        let id: i64 = tx.query_row(
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
    pub fn get_task(&self, id: i64) -> Result<Option<TaskRecord>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_task)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// Fetch a task by its source identity.
    pub fn find_by_source(
        &self,
        source: &str,
        source_task_id: &str,
    ) -> Result<Option<TaskRecord>, StateError> {
        let sql =
            format!("SELECT {TASK_COLUMNS} FROM tasks WHERE source = ?1 AND source_task_id = ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![source, source_task_id], row_to_task)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// All tasks, newest first.
    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY id DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_task)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Tasks currently in `state` (used by `status` and slot rebuild).
    pub fn tasks_in_state(&self, state: TaskState) -> Result<Vec<TaskRecord>, StateError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE state = ?1 ORDER BY id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![state.as_str()], row_to_task)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Apply a state-machine event to a task, recording an audit event.
    ///
    /// Sets `finished_at` on entering a terminal state and clears it otherwise
    /// (e.g. on retry). Returns the new state, or an error for an illegal
    /// transition (the DB is left unchanged).
    pub fn apply_event(
        &self,
        id: i64,
        event: TaskEvent,
        detail: Option<serde_json::Value>,
    ) -> Result<TaskState, StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(serde_json::to_string).transpose()?;
        // Update + audit event in one transaction: state never advances
        // without its recorded event (F-72).
        let tx = self.conn.unchecked_transaction()?;
        let to = apply_event_tx(&tx, &now, id, event, detail.as_deref())?;
        tx.commit()?;
        Ok(to)
    }

    /// Record the selected repository for a task (F-14 confirmation result).
    pub fn set_repo(&self, id: i64, repo: &str) -> Result<(), StateError> {
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
        id: i64,
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

    /// Persist the session id returned by `task/dispatch` (F-37), linking it to
    /// its task and the owning plugin.
    ///
    /// Appends a new row rather than replacing, so a retried task keeps its
    /// session history; [`latest_session`](Self::latest_session) exposes the
    /// newest one as the re-attach target. Returns the new row id.
    pub fn record_session(
        &self,
        task_id: i64,
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
    pub fn reserve_session(&self, task_id: i64, plugin: &str) -> Result<i64, StateError> {
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
            return Err(StateError::NotFound(session_row_id));
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
    pub fn latest_session(&self, task_id: i64) -> Result<Option<SessionRecord>, StateError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 \
             ORDER BY created_at DESC, id DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![task_id], row_to_session)?;
        rows.next().transpose().map_err(StateError::from)
    }

    /// All sessions for a task, newest first (session history for `status`).
    pub fn list_sessions(&self, task_id: i64) -> Result<Vec<SessionRecord>, StateError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 \
             ORDER BY created_at DESC, id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![task_id], row_to_session)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// All audit events for a task, oldest first (F-72; `task show` history).
    pub fn list_events(&self, task_id: i64) -> Result<Vec<EventRecord>, StateError> {
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

    /// Count of audit events recorded for a task (F-72).
    pub fn event_count(&self, id: i64) -> Result<i64, StateError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE task_id = ?1",
            params![id],
            |r| r.get(0),
        )?)
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
        detail: Option<serde_json::Value>,
    ) -> Result<(TaskMessageOutcome, Option<TaskState>), StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(serde_json::to_string).transpose()?;
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
            Some(apply_event_tx(
                &tx,
                &now,
                msg.task_id,
                TaskEvent::Reopen,
                detail.as_deref(),
            )?)
        } else {
            None
        };
        tx.commit()?;
        Ok((TaskMessageOutcome::New, reopened))
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
    /// Returns the new state and how many messages came back.
    pub fn retry_task(
        &self,
        id: i64,
        detail: Option<serde_json::Value>,
    ) -> Result<(TaskState, usize), StateError> {
        let now = self.clock.now_rfc3339();
        let detail = detail.as_ref().map(serde_json::to_string).transpose()?;
        let tx = self.conn.unchecked_transaction()?;
        let to = apply_event_tx(&tx, &now, id, TaskEvent::Retry, detail.as_deref())?;
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
        Ok((to, requeued))
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
    ) -> Result<Vec<i64>, StateError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT t.id FROM tasks t \
             JOIN task_messages m ON m.task_id = t.id \
             WHERE t.state = ?1 AND m.processed_at IS NULL \
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![state.as_str()], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// The conversation's undispatched messages, oldest first — its queue.
    ///
    /// Ordered by `id` rather than `received_at`: arrival order is what the
    /// agent should read them in, and `id` gives it without depending on
    /// timestamp resolution.
    pub fn pending_task_messages(&self, task_id: i64) -> Result<Vec<TaskMessage>, StateError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_MESSAGE_COLUMNS} FROM task_messages \
             WHERE task_id = ?1 AND processed_at IS NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![task_id], row_to_task_message)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(StateError::from)
    }

    /// Every message of a conversation, oldest first (display).
    pub fn list_task_messages(&self, task_id: i64) -> Result<Vec<TaskMessage>, StateError> {
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
    pub fn mark_messages_processed(&self, task_id: i64) -> Result<String, StateError> {
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
    pub fn unprocess_last_batch(&self, task_id: i64) -> Result<usize, StateError> {
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
    pub fn unknown_stop_streak(&self, task_id: i64) -> Result<u32, StateError> {
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

    /// Bump a task's `last_signal_at` to now — the R-10 timeout anchor.
    pub fn touch_last_signal(&self, task_id: i64) -> Result<(), StateError> {
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
            return Err(StateError::NotFound(session_row_id));
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

/// Map a `tasks` row (in [`TASK_COLUMNS`] order) to a [`TaskRecord`].
fn row_to_task(row: &Row<'_>) -> rusqlite::Result<TaskRecord> {
    let state_str: String = row.get("state")?;
    let state = state_str
        .parse::<TaskState>()
        .map_err(|e| conversion_error(Box::new(e)))?;
    let payload: Option<String> = row.get("source_payload")?;
    let source_payload = payload
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| conversion_error(Box::new(e)))?;

    Ok(TaskRecord {
        id: row.get("id")?,
        source: row.get("source")?,
        source_task_id: row.get("source_task_id")?,
        workflow: row.get("workflow")?,
        mode: row.get("mode")?,
        repo: row.get("repo")?,
        worktree_path: row.get("worktree_path")?,
        branch: row.get("branch")?,
        base_commit: row.get("base_commit")?,
        state,
        priority: row.get("priority")?,
        title: row.get("title")?,
        url: row.get("url")?,
        source_payload,
        finished_at: row.get("finished_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        last_signal_at: row.get("last_signal_at")?,
    })
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
fn unprocess_last_batch_tx(conn: &Connection, task_id: i64) -> Result<usize, StateError> {
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
fn apply_event_tx(
    conn: &Connection,
    now: &str,
    id: i64,
    event: TaskEvent,
    detail: Option<&str>,
) -> Result<TaskState, StateError> {
    let from: Option<String> = conn
        .query_row("SELECT state FROM tasks WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    let from: TaskState = from.ok_or(StateError::NotFound(id))?.parse()?;
    let to = transition(from, event)?;
    let finished_at = to.is_terminal().then(|| now.to_string());
    conn.execute(
        "UPDATE tasks SET state = ?1, updated_at = ?2, finished_at = ?3 WHERE id = ?4",
        params![to.as_str(), now, finished_at, id],
    )?;
    conn.execute(
        "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, from.as_str(), to.as_str(), now, detail],
    )?;
    Ok(to)
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
    use super::*;
    use crate::adapters::clock::ManualClock;

    /// Real-clock RFC 3339 timestamp for direct-INSERT helpers whose exact
    /// value is irrelevant.
    fn now() -> String {
        SystemClock.now_rfc3339()
    }

    /// Fixed test epoch (#174).
    const T0: &str = "2026-01-01T00:00:00Z";

    /// A manually driven clock frozen at [`T0`], for exact-timestamp asserts.
    fn manual_clock() -> Arc<ManualClock> {
        let t0 = time::OffsetDateTime::parse(T0, &time::format_description::well_known::Rfc3339)
            .unwrap();
        Arc::new(ManualClock::new(t0))
    }

    fn sample_task() -> NewTask {
        NewTask {
            source: "github".to_string(),
            source_task_id: "42".to_string(),
            workflow: "implement".to_string(),
            mode: "implement".to_string(),
            repo: None,
            priority: 0,
            title: "Fix the bug".to_string(),
            url: Some("https://example.com/issues/42".to_string()),
            source_payload: Some(serde_json::json!({"labels": ["bug"]})),
            last_signal_at: None,
        }
    }

    /// A hook event with empty idempotency components (the common case: the
    /// `(job_id, event)` pair carries the key).
    fn hook_event(
        task_id: i64,
        job_id: &str,
        event: &str,
        status: Option<&str>,
    ) -> HookEventInsert {
        HookEventInsert {
            job_id: job_id.to_string(),
            task_id,
            tool_session_id: String::new(),
            prompt_id: String::new(),
            event: event.to_string(),
            status: status.map(str::to_string),
            payload: "{}".to_string(),
        }
    }

    fn message(task_id: i64, key: &str, body: &str) -> TaskMessageInsert {
        TaskMessageInsert {
            task_id,
            message_key: key.to_string(),
            author: Some("tomoya".to_string()),
            body: body.to_string(),
            url: Some(format!("https://example.com/{key}")),
            payload: format!(r#"{{"id":"conv","message_key":"{key}"}}"#),
        }
    }

    fn keys(messages: &[TaskMessage]) -> Vec<&str> {
        messages.iter().map(|m| m.message_key.as_str()).collect()
    }

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
                source_task_id: "43".to_string(),
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
            db.apply_event(id, TaskEvent::Dispatch, None).unwrap(),
            TaskState::Dispatched
        );
        db.apply_event(id, TaskEvent::Start, None).unwrap();
        db.apply_event(id, TaskEvent::BeginPublish, None).unwrap();
        clock.advance(time::Duration::seconds(90));
        let final_state = db
            .apply_event(id, TaskEvent::Complete, Some(serde_json::json!({"pr": 7})))
            .unwrap();
        assert_eq!(final_state, TaskState::Done);

        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Done);
        assert_eq!(
            rec.finished_at.as_deref(),
            Some("2026-01-01T00:01:30Z"),
            "the terminal transition stamps finished_at from the clock"
        );
        assert_eq!(rec.created_at, T0);
        // 1 ingest + 4 transitions.
        assert_eq!(db.event_count(id).unwrap(), 5);
    }

    #[test]
    fn illegal_transition_leaves_db_unchanged() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        // Cannot Start straight from Queued.
        assert!(db.apply_event(id, TaskEvent::Start, None).is_err());
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Queued);
        assert_eq!(db.event_count(id).unwrap(), 1); // only ingest
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
            db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
            db.apply_event(id, TaskEvent::Start, None).unwrap();
            id
        }; // db dropped, simulating process exit

        let db = StateDb::open(&path).unwrap();
        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Running);
        assert_eq!(rec.source_task_id, "42");
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
            db.set_repo(999, "totsuka").unwrap_err(),
            StateError::NotFound(999)
        ));
        assert!(matches!(
            db.set_worktree(999, "/tmp/wt", Some("b"), "c0ffee")
                .unwrap_err(),
            StateError::NotFound(999)
        ));
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

        // Latest (highest id on a created_at tie) is the re-attach target.
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

    #[test]
    fn record_session_rejects_unknown_task() {
        let db = StateDb::open_in_memory().unwrap();
        assert!(matches!(
            db.record_session(999, "herdr", "sess-x").unwrap_err(),
            StateError::NotFound(999)
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
            db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
            db.apply_event(id, TaskEvent::Start, None).unwrap();
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
    fn list_events_returns_full_history_in_order() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
        db.apply_event(id, TaskEvent::Start, Some(serde_json::json!({"k": 1})))
            .unwrap();

        let events = db.list_events(id).unwrap();
        assert_eq!(events.len(), 3);
        // Ingest first: no from_state, lands in queued.
        assert_eq!(events[0].from_state, None);
        assert_eq!(events[0].to_state, TaskState::Queued);
        assert_eq!(events[1].from_state, Some(TaskState::Queued));
        assert_eq!(events[1].to_state, TaskState::Dispatched);
        assert_eq!(events[2].to_state, TaskState::Running);
        assert_eq!(events[2].detail, Some(serde_json::json!({"k": 1})));
        // Unknown task -> empty history, not an error.
        assert!(db.list_events(999).unwrap().is_empty());
    }

    #[test]
    fn every_event_detail_is_valid_json_or_null() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();
        db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
        db.apply_event(id, TaskEvent::Start, Some(serde_json::json!({"pid": 7})))
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
        let task = db.find_by_source("github", "7").unwrap().unwrap();
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
            task_id: 1,
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
        let rec = db.latest_session(1).unwrap().unwrap();
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
            task_id: 1,
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
        let rec = db.get_task(1).unwrap().unwrap();
        assert_eq!(rec.title, "legacy");
        assert_eq!(rec.state, TaskState::Done);
        assert_eq!(
            db.latest_session(1).unwrap().unwrap().tool_session_id,
            Some("cc-1".to_string())
        );

        // The ledger is usable, and v6 has given this pre-existing task its
        // backfilled row (see `migrates_v5_to_v6_...` for why that matters).
        assert_eq!(keys(&db.list_task_messages(1).unwrap()), ["9"]);
        assert!(db.pending_task_messages(1).unwrap().is_empty());
        assert_eq!(
            db.append_task_message(&message(1, "m1", "hello")).unwrap(),
            TaskMessageOutcome::New
        );
        assert_eq!(keys(&db.list_task_messages(1).unwrap()), ["9", "m1"]);

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
            let ledger = db.list_task_messages(task_id).unwrap();
            assert_eq!(keys(&ledger), [key], "one row per pre-existing task");
            assert!(
                ledger[0].processed_at.is_some(),
                "backfilled rows must not look like queued prompt material"
            );
            assert!(db.pending_task_messages(task_id).unwrap().is_empty());
            // ...so the source's next re-delivery dedups instead of reopening.
            assert_eq!(
                db.append_task_message_reopening(&message(task_id, key, "re-delivered"), None)
                    .unwrap(),
                (TaskMessageOutcome::Duplicate, None)
            );
        }
        assert_eq!(db.get_task(1).unwrap().unwrap().state, TaskState::Done);

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

        let task = db.find_by_source("slack", "C1:100.1").unwrap().unwrap();
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
            db.apply_event(id, event, None).unwrap();
        }
        let before = db.get_task(id).unwrap().unwrap();
        assert!(before.finished_at.is_some());

        let (outcome, reopened) = db
            .append_task_message_reopening(
                &message(id, "m2", "a follow-up"),
                Some(serde_json::json!({"kind": "reopen"})),
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
            db.reserve_session(999, "herdr").unwrap_err(),
            StateError::NotFound(999)
        ));
        assert!(matches!(
            db.set_session_native_id(999, "x").unwrap_err(),
            StateError::NotFound(999)
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
            db.get_task(id).unwrap().unwrap().last_signal_at.as_deref(),
            Some(T0)
        );
        clock.advance(time::Duration::seconds(60));
        db.touch_last_signal(id).unwrap();
        assert_eq!(
            db.get_task(id).unwrap().unwrap().last_signal_at.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );

        // Unknown ids are rejected, matching the other setters' contract.
        assert!(matches!(
            db.touch_last_signal(999).unwrap_err(),
            StateError::NotFound(999)
        ));
        assert!(matches!(
            db.set_tool_session_id(999, "x").unwrap_err(),
            StateError::NotFound(999)
        ));
    }
}
