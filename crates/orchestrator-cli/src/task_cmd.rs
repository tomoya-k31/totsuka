//! `totsuka task ...` — per-task operations (§5.1): list / show / cancel /
//! retry.
//!
//! `cancel` / `retry` are state-machine transitions on the DB (#48); the agent
//! session and slots are reconciled by the next `totsuka run` (recovery/retry
//! reuse, F-44).

use clap::Subcommand;
use orchestrator_core::domain::state::{TaskEvent, TaskState};
use serde::Serialize;
use serde_json::Value;

use crate::common::{CliError, Cx, JsonFlag, print_json};

/// Task subcommands.
#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List all tasks.
    List {
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Show one task: state, sessions, worktree, and full event history.
    Show {
        /// Task id (see `totsuka status`).
        id: i64,
        #[command(flatten)]
        json: JsonFlag,
    },
    /// Cancel a task (any non-finished state).
    Cancel {
        /// Task id.
        id: i64,
    },
    /// Re-queue a failed or cancelled task (reuses its worktree/session when
    /// possible, F-44).
    Retry {
        /// Task id.
        id: i64,
    },
    /// Approve or reject a task awaiting human verification
    /// (`verification = "human"`, #131 D-01).
    Verify {
        /// Task id (must be in the `verifying` state).
        id: i64,
        /// Approve the self-reported completion → publish on the next run.
        #[arg(long, conflicts_with = "fail")]
        pass: bool,
        /// Reject the completion → back to running for correction in the pane.
        #[arg(long, requires = "reason")]
        fail: bool,
        /// Reason for rejection (required with `--fail`).
        #[arg(long)]
        reason: Option<String>,
    },
}

impl TaskCommand {
    /// Whether this subcommand was invoked with `--json` (drives the JSON
    /// error envelope in `main`).
    pub fn wants_json(&self) -> bool {
        matches!(
            self,
            Self::List { json } | Self::Show { json, .. } if json.json
        )
    }
}

/// A `task show --json` document.
#[derive(Debug, Serialize)]
struct TaskDetail {
    id: i64,
    source: String,
    source_task_id: String,
    workflow: String,
    mode: String,
    state: String,
    repo: Option<String>,
    priority: i64,
    title: String,
    url: Option<String>,
    worktree_path: Option<String>,
    branch: Option<String>,
    finished_at: Option<String>,
    created_at: String,
    updated_at: String,
    sessions: Vec<SessionRow>,
    events: Vec<EventRow>,
}

#[derive(Debug, Serialize)]
struct SessionRow {
    plugin: String,
    session_id: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct EventRow {
    from: Option<String>,
    to: String,
    occurred_at: String,
    detail: Option<Value>,
}

/// Dispatch a task subcommand.
pub fn run(cx: &Cx, command: TaskCommand) -> Result<(), CliError> {
    match command {
        TaskCommand::List { json } => list(cx, json.json),
        TaskCommand::Show { id, json } => show(cx, id, json.json),
        TaskCommand::Cancel { id } => cancel(cx, id),
        TaskCommand::Retry { id } => retry(cx, id),
        TaskCommand::Verify {
            id,
            pass,
            fail,
            reason,
        } => verify(cx, id, pass, fail, reason),
    }
}

fn list(cx: &Cx, json: bool) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let tasks = db.list_tasks()?;
    if json {
        let rows: Vec<Value> = tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id, "state": t.state.to_string(), "source": t.source,
                    "source_task_id": t.source_task_id, "workflow": t.workflow,
                    "repo": t.repo, "title": t.title, "updated_at": t.updated_at,
                })
            })
            .collect();
        return print_json(&rows);
    }
    if tasks.is_empty() {
        println!("no tasks yet → `totsuka run` ingests them from your task sources");
        return Ok(());
    }
    println!(
        "{:<5} {:<14} {:<10} {:<12} TITLE",
        "ID", "STATE", "SOURCE", "WORKFLOW"
    );
    for t in &tasks {
        println!(
            "{:<5} {:<14} {:<10} {:<12} {}",
            t.id, t.state, t.source, t.workflow, t.title
        );
    }
    Ok(())
}

fn show(cx: &Cx, id: i64, json: bool) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    let detail = TaskDetail {
        id: task.id,
        source: task.source,
        source_task_id: task.source_task_id,
        workflow: task.workflow,
        mode: task.mode,
        state: task.state.to_string(),
        repo: task.repo,
        priority: task.priority,
        title: task.title,
        url: task.url,
        worktree_path: task.worktree_path,
        branch: task.branch,
        finished_at: task.finished_at,
        created_at: task.created_at,
        updated_at: task.updated_at,
        sessions: db
            .list_sessions(id)?
            .into_iter()
            .map(|s| SessionRow {
                plugin: s.plugin,
                session_id: s.session_id,
                created_at: s.created_at,
            })
            .collect(),
        events: db
            .list_events(id)?
            .into_iter()
            .map(|e| EventRow {
                from: e.from_state.map(|s| s.to_string()),
                to: e.to_state.to_string(),
                occurred_at: e.occurred_at,
                detail: e.detail,
            })
            .collect(),
    };
    if json {
        return print_json(&detail);
    }
    println!("task {}: {}", detail.id, detail.title);
    println!("  state:     {}", detail.state);
    println!("  source:    {}#{}", detail.source, detail.source_task_id);
    println!("  workflow:  {} (mode {})", detail.workflow, detail.mode);
    println!("  repo:      {}", detail.repo.as_deref().unwrap_or("-"));
    if let Some(url) = &detail.url {
        println!("  url:       {url}");
    }
    if let Some(path) = &detail.worktree_path {
        println!(
            "  worktree:  {path} [{}]",
            detail.branch.as_deref().unwrap_or("-")
        );
    }
    if !detail.sessions.is_empty() {
        println!("  sessions (newest first):");
        for s in &detail.sessions {
            println!("    {} @ {} ({})", s.session_id, s.plugin, s.created_at);
        }
    }
    println!("  history:");
    for e in &detail.events {
        let from = e.from.as_deref().unwrap_or("(ingest)");
        println!("    {} {} → {}", e.occurred_at, from, e.to);
    }
    Ok(())
}

fn cancel(cx: &Cx, id: i64) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if task.state.is_terminal() {
        return Err(format!(
            "task {id} is already {} → nothing to cancel; use `totsuka task retry {id}` to re-run it",
            task.state
        )
        .into());
    }
    db.apply_event(
        id,
        TaskEvent::Cancel,
        Some(serde_json::json!({ "kind": "cli", "command": "task cancel" })),
    )?;
    println!("task {id} cancelled");
    if matches!(
        task.state,
        TaskState::Dispatched
            | TaskState::Running
            | TaskState::WaitingInput
            | TaskState::Publishing
    ) {
        println!(
            "note: its agent session is reconciled on the next `totsuka run`; the worktree is kept per the cleanup policy"
        );
    }
    Ok(())
}

fn retry(cx: &Cx, id: i64) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if !matches!(task.state, TaskState::Failed | TaskState::Cancelled) {
        let action = if task.state == TaskState::Done {
            "completed tasks are not re-run; create a new task at the source instead"
        } else {
            "only failed/cancelled tasks can be retried; `totsuka task cancel` it first if you want a re-run"
        };
        return Err(format!("task {id} is {} → {action}", task.state).into());
    }
    // `retry_task`, not `apply_event(Retry)`: requeueing the task without the
    // messages its failed run was given would dispatch an empty prompt (#242).
    let (_, requeued) = db.retry_task(
        id,
        Some(serde_json::json!({ "kind": "cli", "command": "task retry" })),
    )?;
    println!(
        "task {id} re-queued → `totsuka run` dispatches it (reusing its worktree/session when possible)"
    );
    if requeued > 0 {
        println!("  {requeued} message(s) from the last dispatch will be sent again");
    }
    Ok(())
}

fn verify(
    cx: &Cx,
    id: i64,
    pass: bool,
    fail: bool,
    reason: Option<String>,
) -> Result<(), CliError> {
    let db = cx.open_state_db()?;
    let task = db.get_task(id)?.ok_or_else(|| not_found(id))?;
    if task.state != TaskState::Verifying {
        return Err(format!(
            "task {id} is {} → `totsuka task verify` applies only to a task awaiting human verification (state `verifying`)",
            task.state
        )
        .into());
    }
    if pass {
        // ApproveVerification → Publishing; the next `totsuka run` recover cycle
        // finalizes it via the existing Publishing-restore path (#131 D-01).
        db.apply_event(
            id,
            TaskEvent::ApproveVerification,
            Some(serde_json::json!({ "kind": "cli", "command": "task verify --pass" })),
        )?;
        println!("task {id} verification passed → `totsuka run` publishes it on the next cycle");
    } else if fail {
        // VerificationFailed → Running; the human gives corrective instructions
        // directly in the agent pane (D-07).
        let reason = reason.unwrap_or_default();
        db.apply_event(
            id,
            TaskEvent::VerificationFailed,
            Some(serde_json::json!({
                "kind": "cli", "command": "task verify --fail", "reason": reason,
            })),
        )?;
        println!(
            "task {id} verification failed → back to running; give corrective instructions in the agent pane"
        );
    } else {
        return Err(
            "specify --pass to approve, or --fail --reason <text> to reject → see `totsuka task verify --help`"
                .into(),
        );
    }
    Ok(())
}

fn not_found(id: i64) -> CliError {
    format!("task {id} not found → `totsuka task list` shows known ids").into()
}
