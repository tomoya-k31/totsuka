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
//!   first when there are pending migrations (§10.3).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Row, params};

use crate::domain::state::{InvalidTransition, TaskEvent, TaskState, UnknownState, transition};

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
];

/// Columns of `tasks`, in the order [`row_to_task`] reads them.
const TASK_COLUMNS: &str = "id, source, source_task_id, workflow, mode, repo, \
     worktree_path, branch, state, priority, title, url, source_payload, \
     finished_at, created_at, updated_at";

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
}

/// The SQLite state database.
pub struct StateDb {
    conn: Connection,
}

impl StateDb {
    /// Open (creating if needed) a file-backed state DB and run migrations.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let preexisting = path.exists();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some((path.to_path_buf(), preexisting)))
    }

    /// Open an ephemeral in-memory DB (tests).
    pub fn open_in_memory() -> Result<Self, StateError> {
        Self::init(Connection::open_in_memory()?, None)
    }

    /// Shared init: enable FKs, run migrations (with backup if pending).
    fn init(mut conn: Connection, backup: Option<(PathBuf, bool)>) -> Result<Self, StateError> {
        // rusqlite defaults foreign_keys OFF; the schema declares FKs.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )?;
        let current: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?;

        if (current as usize) < MIGRATIONS.len() {
            // Back up the DB file before mutating its schema (§10.3).
            if let Some((path, true)) = &backup {
                let bak = PathBuf::from(format!("{}.bak", path.display()));
                fs::copy(path, bak)?;
            }
            for (i, sql) in MIGRATIONS.iter().enumerate() {
                let version = (i + 1) as i64;
                if version > current {
                    let tx = conn.transaction()?;
                    tx.execute_batch(sql)?;
                    tx.execute(
                        "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                        params![version, now()],
                    )?;
                    tx.commit()?;
                }
            }
        }
        Ok(Self { conn })
    }

    /// Ingest a task idempotently (F-73). Returns its id, whether newly
    /// inserted or already present under the same `(source, source_task_id)`.
    pub fn upsert_task(&self, task: &NewTask) -> Result<i64, StateError> {
        let now = now();
        let payload = task
            .source_payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let changed = self.conn.execute(
            "INSERT INTO tasks
                (source, source_task_id, workflow, mode, repo, state, priority,
                 title, url, source_payload, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)
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
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM tasks WHERE source = ?1 AND source_task_id = ?2",
            params![task.source, task.source_task_id],
            |r| r.get(0),
        )?;
        if changed > 0 {
            // Ingest event: from_state NULL -> queued (F-72).
            self.conn.execute(
                "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
                 VALUES (?1, NULL, ?2, ?3, ?4)",
                params![id, TaskState::Queued.as_str(), now, "ingested"],
            )?;
        }
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
        let record = self.get_task(id)?.ok_or(StateError::NotFound(id))?;
        let to = transition(record.state, event)?;
        let now = now();
        let finished_at = to.is_terminal().then(|| now.clone());
        let detail = detail.as_ref().map(serde_json::to_string).transpose()?;

        self.conn.execute(
            "UPDATE tasks SET state = ?1, updated_at = ?2, finished_at = ?3 WHERE id = ?4",
            params![to.as_str(), now, finished_at, id],
        )?;
        self.conn.execute(
            "INSERT INTO events (task_id, from_state, to_state, occurred_at, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, record.state.as_str(), to.as_str(), now, detail],
        )?;
        Ok(to)
    }

    /// Record the selected repository for a task (F-14 confirmation result).
    pub fn set_repo(&self, id: i64, repo: &str) -> Result<(), StateError> {
        self.conn.execute(
            "UPDATE tasks SET repo = ?1, updated_at = ?2 WHERE id = ?3",
            params![repo, now(), id],
        )?;
        Ok(())
    }

    /// Record the worktree path and branch for a task (#53).
    pub fn set_worktree(&self, id: i64, path: &str, branch: &str) -> Result<(), StateError> {
        self.conn.execute(
            "UPDATE tasks SET worktree_path = ?1, branch = ?2, updated_at = ?3 WHERE id = ?4",
            params![path, branch, now(), id],
        )?;
        Ok(())
    }

    /// Count of audit events recorded for a task (F-72).
    pub fn event_count(&self, id: i64) -> Result<i64, StateError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE task_id = ?1",
            params![id],
            |r| r.get(0),
        )?)
    }
}

/// Current time as an ISO 8601 (RFC 3339) UTC string.
fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
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
        state,
        priority: row.get("priority")?,
        title: row.get("title")?,
        url: row.get("url")?,
        source_payload,
        finished_at: row.get("finished_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Wrap a domain error as a rusqlite column-conversion failure.
fn conversion_error(e: Box<dyn std::error::Error + Send + Sync>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
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
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&sample_task()).unwrap();

        assert_eq!(
            db.apply_event(id, TaskEvent::Dispatch, None).unwrap(),
            TaskState::Dispatched
        );
        db.apply_event(id, TaskEvent::Start, None).unwrap();
        db.apply_event(id, TaskEvent::BeginPublish, None).unwrap();
        let final_state = db
            .apply_event(id, TaskEvent::Complete, Some(serde_json::json!({"pr": 7})))
            .unwrap();
        assert_eq!(final_state, TaskState::Done);

        let rec = db.get_task(id).unwrap().unwrap();
        assert_eq!(rec.state, TaskState::Done);
        assert!(rec.finished_at.is_some(), "terminal state sets finished_at");
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
        db.set_worktree(id, "/tmp/wt", "agent/github-42").unwrap();

        let queued = db.tasks_in_state(TaskState::Queued).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].repo.as_deref(), Some("totsuka"));
        assert_eq!(queued[0].branch.as_deref(), Some("agent/github-42"));
        assert!(db.tasks_in_state(TaskState::Running).unwrap().is_empty());
    }
}
