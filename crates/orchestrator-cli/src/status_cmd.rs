//! `totsuka status` — task/worktree overview + orchestrator liveness (§5.1).
//!
//! Read-only: SQLite is read directly, no plugin is launched (§5.5 keeps this
//! under 500 ms at the target scale).

use orchestrator_core::adapters::state_db::TaskNote;
use orchestrator_core::agent_tools;
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
    /// Why this task is not moving, if the orchestrator recorded a reason
    /// (#407). `None` for every task that is simply getting on with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_reason: Option<WaitReason>,
}

/// The rendered form of an unresolved [`TaskNote`] (#407).
#[derive(Debug, Serialize)]
struct WaitReason {
    /// The note's kind, so a script can branch on it without parsing prose.
    kind: String,
    /// When the orchestrator recorded it (ISO 8601 UTC).
    since: String,
    /// One line for a human, including what to do about it.
    message: String,
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
    let mut notes = db.open_notes()?;
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
            wait_reason: notes.remove(&t.id).map(wait_reason),
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

    // A block of its own rather than an extra column: the reason is a
    // sentence with a remedy in it, and the table's last column is the
    // source-controlled title.
    let blocked: Vec<&TaskRow> = tasks.iter().filter(|t| t.wait_reason.is_some()).collect();
    if !blocked.is_empty() {
        println!("\nnot starting yet:");
        for t in blocked {
            let reason = t.wait_reason.as_ref().expect("filtered on Some");
            // The message is assembled from a `detail` read back out of
            // SQLite. Today every part of it is ours, but "ours" is a
            // property of the writer, not of this print site — and #280's
            // rule is that the human rendering defuses, while `--json` stays
            // byte-exact. Hence here and not inside `wait_reason`.
            println!(
                "  task {} ({}): {}",
                t.id,
                safe(&reason.since),
                safe(&reason.message)
            );
        }
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

/// Render a recorded note into the line `status` shows (#407).
///
/// The prose is rebuilt here from the note's structured fields rather than
/// stored with it, so an upgraded binary explains an old note with its current
/// wording. A kind this build does not know still gets a row — saying "task 4
/// is blocked, kind `x`" beats silently dropping it, which would read as "not
/// blocked".
///
/// Nothing is sanitised here: this feeds `--json` too, which must stay
/// byte-exact (#280). Defusing happens at the print site.
fn wait_reason(note: TaskNote) -> WaitReason {
    let message = match note.kind.as_str() {
        agent_tools::BLOCKED_NOTE => {
            let missing: Vec<&str> = note
                .detail
                .get("missing")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            agent_tools::blocked_reason(&missing)
        }
        other => format!("blocked: {other} — see `totsuka task show`"),
    };
    WaitReason {
        kind: note.kind,
        since: note.since,
        message,
    }
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
