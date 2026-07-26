//! `totsuka status` — task/worktree overview + orchestrator liveness (§5.1).
//!
//! Read-only: SQLite is read directly, no plugin is launched (§5.5 keeps this
//! under 500 ms at the target scale).

use orchestrator_core::domain::state::TaskState;
use orchestrator_core::platform::PlatformProcessProbe;
use orchestrator_core::ports::ProcessProbe;
use serde::Serialize;

use crate::common::{CliError, Cx, print_json, safe};

/// Liveness of the `run` process, from the lock file (F-74).
#[derive(Debug, Serialize)]
struct OrchestratorStatus {
    /// Whether a live `run` holds the lock.
    running: bool,
    /// The lock holder's PID, if a lock file exists.
    pid: Option<u32>,
    /// A lock file exists but its PID is dead (crashed run).
    stale_lock: bool,
}

/// One task row of the status report.
#[derive(Debug, Serialize)]
struct TaskRow {
    id: i64,
    state: String,
    workflow: String,
    repo: Option<String>,
    title: String,
    worktree_path: Option<String>,
    branch: Option<String>,
    updated_at: String,
}

/// The whole `--json` document.
#[derive(Debug, Serialize)]
struct StatusReport {
    orchestrator: OrchestratorStatus,
    tasks: Vec<TaskRow>,
}

/// Execute `totsuka status`.
pub fn run(cx: &Cx, json: bool) -> Result<(), CliError> {
    let orchestrator = lock_status(cx);
    let db = cx.open_state_db()?;
    let tasks: Vec<TaskRow> = db
        .list_tasks()?
        .into_iter()
        .map(|t| TaskRow {
            id: t.id,
            state: t.state.to_string(),
            workflow: t.workflow,
            repo: t.repo,
            title: t.title,
            worktree_path: t.worktree_path,
            branch: t.branch,
            updated_at: t.updated_at,
        })
        .collect();

    if json {
        return print_json(&StatusReport {
            orchestrator,
            tasks,
        });
    }

    match (
        orchestrator.running,
        orchestrator.stale_lock,
        orchestrator.pid,
    ) {
        (true, _, Some(pid)) => println!("orchestrator: running (pid {pid})"),
        (true, _, None) => println!("orchestrator: running"),
        // Stale lock with a dead PID: name it so the user can confirm.
        (false, true, Some(pid)) => println!(
            "orchestrator: not running (stale lock from pid {pid} → it will be reclaimed on the next `totsuka run`)"
        ),
        // Stale lock we could not parse (corrupt/empty file): don't invent a PID.
        (false, true, None) => println!(
            "orchestrator: not running (unreadable lock file → it will be reclaimed on the next `totsuka run`)"
        ),
        (false, false, _) => println!("orchestrator: not running"),
    }

    if tasks.is_empty() {
        println!("no tasks yet → `totsuka run` ingests them from your task sources");
        return Ok(());
    }

    println!(
        "{:<5} {:<14} {:<12} {:<12} TITLE",
        "ID", "STATE", "WORKFLOW", "REPO"
    );
    for t in &tasks {
        // Source-controlled text must not be able to repaint the table (#280).
        println!(
            "{:<5} {:<14} {:<12} {:<12} {}",
            t.id,
            t.state,
            t.workflow,
            t.repo.as_deref().unwrap_or("-"),
            safe(&t.title)
        );
    }

    let waiting = count(&tasks, TaskState::WaitingInput);
    let pending = count(&tasks, TaskState::Pending);
    if waiting > 0 {
        println!("{waiting} task(s) waiting for input → answer in the agent, then `totsuka run`");
    }
    if pending > 0 {
        println!("{pending} task(s) pending repository confirmation → `totsuka task show <id>`");
    }

    let worktrees: Vec<&TaskRow> = tasks.iter().filter(|t| t.worktree_path.is_some()).collect();
    if !worktrees.is_empty() {
        println!("\nworktrees:");
        for t in worktrees {
            println!(
                "  task {} [{}] {}",
                t.id,
                safe(t.branch.as_deref().unwrap_or("-")),
                safe(t.worktree_path.as_deref().unwrap_or("-"))
            );
        }
    }
    Ok(())
}

fn count(tasks: &[TaskRow], state: TaskState) -> usize {
    tasks.iter().filter(|t| t.state == state.as_str()).count()
}

/// Inspect the run lock (F-74): live holder, stale lock, or no lock.
fn lock_status(cx: &Cx) -> OrchestratorStatus {
    let path = cx.paths.state_dir().join("run.lock");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return OrchestratorStatus {
            running: false,
            pid: None,
            stale_lock: false,
        };
    };
    match contents.trim().parse::<u32>() {
        Ok(pid) => {
            let alive = PlatformProcessProbe::default().is_alive(pid);
            OrchestratorStatus {
                running: alive,
                pid: Some(pid),
                stale_lock: !alive,
            }
        }
        Err(_) => OrchestratorStatus {
            running: false,
            pid: None,
            stale_lock: true,
        },
    }
}
