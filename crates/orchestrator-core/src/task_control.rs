//! The rules for cancelling and retrying one task (#760).
//!
//! Two callers apply them: the running engine, answering `POST /task/cancel`
//! / `POST /task/retry` on the control socket, and the CLI writing the state
//! DB directly when no engine answers. Both must refuse the same requests with
//! the same advice, so the rules live here rather than in either of them. The
//! state machine itself stays in [`transition`](crate::domain::state::transition);
//! this only decides which event to ask for and phrases the refusals.

use std::path::{Path, PathBuf};

use crate::adapters::state_db::{StateDb, StateError};
use crate::domain::state::{TaskEvent, TaskState};
use crate::domain::{EventDetail, Task, TaskId};
use crate::ports::git::GitRunner;
use crate::ports::signal_ingress::TaskControlOutcome;
use crate::worktree::{CleanupOutcome, WorktreeManager};

/// Cancel task `id`. `detail` is the audit detail recorded with the event.
///
/// Only the DB side: the engine's in-memory bookkeeping (slot, session
/// routes) is the engine's to release after an applied cancel.
pub fn cancel(
    db: &StateDb,
    id: TaskId,
    detail: EventDetail,
) -> Result<TaskControlOutcome, StateError> {
    let Some(task) = db.get_task(id)? else {
        return Ok(TaskControlOutcome::refused(not_found(id)));
    };
    if task.state.is_terminal() {
        // The advice has to match what `retry` actually accepts: it refuses a
        // `done` task, and since #242 the way to carry a finished conversation
        // forward is another message in it, not a re-run of the old one.
        let next = if task.state == TaskState::Done {
            "it finished; send another message in the conversation (the reply in its thread/issue) to continue it".to_string()
        } else {
            format!("use `totsuka task retry {id}` to re-run it")
        };
        return Ok(TaskControlOutcome::refused(format!(
            "task {id} is already {} → nothing to cancel; {next}",
            task.state
        )));
    }
    let to = match db.apply_event(task.task_ref(), TaskEvent::Cancel, Some(detail)) {
        Ok((to, _)) => to,
        Err(e @ (StateError::Conflict(_) | StateError::Transition(_))) => {
            return Ok(lost_race(id, &e));
        }
        Err(e) => return Err(e),
    };
    Ok(TaskControlOutcome::applied(task.state, to, None))
}

/// Retry task `id`. `detail` is the audit detail recorded with the event.
pub fn retry(
    db: &StateDb,
    id: TaskId,
    detail: EventDetail,
) -> Result<TaskControlOutcome, StateError> {
    match retryable(db, id)? {
        // `retry_task`, not `apply_event(Retry)`: requeueing the task without
        // the messages its failed run was given would dispatch an empty
        // prompt (#242).
        Ok(task) => settle(&task, db.retry_task(task.task_ref(), Some(detail))),
        Err(refused) => Ok(refused),
    }
}

/// Retry task `id` in repository `repo` instead of the one it was given
/// (#896) — for a task whose repository was chosen wrong, which a plain retry
/// would only re-run in the same wrong place.
///
/// `command` is what the operator ran, recorded on the
/// [`Moved`](EventDetail::Moved) event. `repos` is the configured
/// `[[repositories]]` as `(name, path)`. The old
/// worktree is removed first, under the same guards as cleanup (uncommitted
/// changes, unpushed commits on a detached HEAD): a refusal leaves the task
/// as it was. Its pane is left to the next dispatch, which closes a previous
/// dispatch's pane before opening one (#481). The caller sends a retry that
/// names the task's own repository to [`retry`] instead.
pub fn retry_in_repo<G: GitRunner>(
    db: &StateDb,
    id: TaskId,
    command: &str,
    repo: &str,
    repos: &[(String, PathBuf)],
    worktrees: &WorktreeManager<G>,
) -> Result<TaskControlOutcome, StateError> {
    let task = match retryable(db, id)? {
        Ok(task) => task,
        Err(refused) => return Ok(refused),
    };
    if !repos.iter().any(|(name, _)| name == repo) {
        let known: Vec<_> = repos.iter().map(|(name, _)| format!("`{name}`")).collect();
        return Ok(TaskControlOutcome::refused(format!(
            "repository `{repo}` is not in [[repositories]] → choose one of: {}",
            known.join(", ")
        )));
    }
    if let Err(reason) = vacate_worktree(&task, repos, worktrees) {
        return Ok(TaskControlOutcome::refused(reason));
    }
    settle(&task, db.retry_task_in_repo(task.task_ref(), command, repo))
}

/// The task, if it is in a state `retry` accepts; the refusal otherwise.
fn retryable(db: &StateDb, id: TaskId) -> Result<Result<Task, TaskControlOutcome>, StateError> {
    let Some(task) = db.get_task(id)? else {
        return Ok(Err(TaskControlOutcome::refused(not_found(id))));
    };
    if matches!(
        task.state,
        TaskState::Failed | TaskState::Cancelled | TaskState::Skipped
    ) {
        return Ok(Ok(task));
    }
    let action = if task.state == TaskState::Done {
        // Since #242 `done` means "no unprocessed messages", not "closed
        // forever": a new message reopens the conversation. Re-running the
        // same instructions is a different thing, and not what anyone
        // asking about a finished task wants.
        "it finished; send another message in the conversation (the reply in its thread/issue) to continue it — a re-run of the same instructions is not what `retry` is for"
    } else {
        "only failed/cancelled/skipped tasks can be retried; `totsuka task cancel` it first if you want a re-run"
    };
    Ok(Err(TaskControlOutcome::refused(format!(
        "task {id} is {} → {action}",
        task.state
    ))))
}

/// The outcome of a requeue of `task` that `applied` reports.
fn settle(
    task: &Task,
    applied: Result<(TaskState, crate::adapters::state_db::TaskRef, usize), StateError>,
) -> Result<TaskControlOutcome, StateError> {
    let (to, requeued) = match applied {
        Ok((to, _, requeued)) => (to, requeued),
        Err(e @ (StateError::Conflict(_) | StateError::Transition(_))) => {
            return Ok(lost_race(task.id, &e));
        }
        Err(e) => return Err(e),
    };
    Ok(TaskControlOutcome::applied(task.state, to, Some(requeued)))
}

/// Remove the task's worktree from its old repository, if it is still on
/// disk. `Err` is the refusal to give, with the way to do it by hand.
fn vacate_worktree<G: GitRunner>(
    task: &Task,
    repos: &[(String, PathBuf)],
    worktrees: &WorktreeManager<G>,
) -> Result<(), String> {
    let Some(path) = task
        .worktree_path
        .as_deref()
        .map(Path::new)
        .filter(|p| p.exists())
    else {
        return Ok(());
    };
    let shown = path.display();
    let old_repo = task
        .repo
        .as_deref()
        .and_then(|old| repos.iter().find(|(name, _)| name == old));
    let Some((_, repo_path)) = old_repo else {
        return Err(format!(
            "the old worktree {shown} belongs to a repository that is not in [[repositories]] \
             → remove it with `git worktree remove {shown}` and retry"
        ));
    };
    match worktrees.remove(
        repo_path,
        path,
        task.branch.as_deref(),
        task.base_commit.as_deref(),
    ) {
        Ok(CleanupOutcome::Removed | CleanupOutcome::Gone) => Ok(()),
        Ok(CleanupOutcome::DirtySkipped | CleanupOutcome::Retained) => Err(format!(
            "the old worktree {shown} has uncommitted changes or unpushed commits → save what \
             you need, remove it with `git worktree remove --force {shown}`, and retry"
        )),
        Err(e) => Err(format!(
            "could not remove the old worktree {shown}: {e} → remove it with \
             `git worktree remove {shown}` and retry"
        )),
    }
}

/// The state moved between the check above and the write — another writer
/// (the CLI writing the DB directly, until #760's CLI switch-over) got there
/// first. Since #763 that arrives as [`StateError::Conflict`]; `Transition`
/// can no longer mean a race here, only a refusal the checks above missed,
/// and is answered the same way rather than stopping `run`. A refusal, not an error: inside the engine an `Err` is run-fatal,
/// and losing this race must not stop `run`.
fn lost_race(id: TaskId, e: &impl std::fmt::Display) -> TaskControlOutcome {
    TaskControlOutcome::refused(format!(
        "task {id} changed state while this was being applied ({e}) → `totsuka task show {id}` and try again"
    ))
}

/// The refusal for an id the DB does not know.
pub fn not_found(id: TaskId) -> String {
    format!("task {id} not found → `totsuka task list` shows known ids")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::state_db::NewTask;
    use crate::domain::SourceTaskId;
    use crate::domain::WorkflowMode;

    fn db_with_task() -> (StateDb, i64) {
        let db = StateDb::open_in_memory().unwrap();
        let id = db
            .upsert_task(&NewTask {
                source: "github".to_string(),
                source_task_id: SourceTaskId("42".to_string()),
                workflow: "implement".to_string(),
                mode: WorkflowMode::Implement,
                repo: Some("web".to_string()),
                priority: 0,
                title: "Fix the bug".to_string(),
                url: None,
                source_payload: None,
                last_signal_at: None,
            })
            .unwrap();
        (db, id.0)
    }

    fn control() -> EventDetail {
        EventDetail::Control {
            command: "test".to_string(),
        }
    }

    #[test]
    fn cancel_then_retry_round_trips_and_reports_where_it_came_from() {
        let (db, id) = db_with_task();
        let cancelled = cancel(&db, TaskId(id), control()).unwrap();
        assert_eq!(
            cancelled,
            TaskControlOutcome::applied(TaskState::Queued, TaskState::Cancelled, None)
        );
        let retried = retry(&db, TaskId(id), control()).unwrap();
        assert_eq!(
            retried,
            TaskControlOutcome::applied(TaskState::Cancelled, TaskState::Queued, Some(0))
        );
    }

    /// A git that answers `status --porcelain` with `status` and everything
    /// else with success, recording what it was asked.
    struct FakeGit {
        status: &'static str,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl GitRunner for &FakeGit {
        fn run(&self, _cwd: &Path, args: &[&str]) -> std::io::Result<crate::ports::git::GitOutput> {
            self.calls.borrow_mut().push(args.join(" "));
            let stdout = if args[0] == "status" { self.status } else { "" };
            Ok(crate::ports::git::GitOutput {
                status: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
            })
        }
    }

    /// A cancelled task in repository `web` with a worktree on disk.
    fn cancelled_with_worktree(name: &str) -> (StateDb, TaskId, PathBuf) {
        let (db, id) = db_with_task();
        let id = TaskId(id);
        let wt = std::env::temp_dir().join(format!("totsuka-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&wt).unwrap();
        db.set_worktree(id, &wt.display().to_string(), None, "abc")
            .unwrap();
        cancel(&db, id, control()).unwrap();
        (db, id, wt)
    }

    fn repos() -> Vec<(String, PathBuf)> {
        vec![
            ("web".to_string(), PathBuf::from("/repos/web")),
            ("api".to_string(), PathBuf::from("/repos/api")),
        ]
    }

    #[test]
    fn retry_in_repo_removes_the_old_worktree_and_moves_the_task() {
        let (db, id, wt) = cancelled_with_worktree("rehome_moves");
        let git = FakeGit {
            status: "",
            calls: Default::default(),
        };
        let outcome = retry_in_repo(
            &db,
            id,
            "test --repo",
            "api",
            &repos(),
            &WorktreeManager::new(&git),
        )
        .unwrap();
        assert!(outcome.ok, "{outcome:?}");
        assert!(
            git.calls
                .borrow()
                .iter()
                .any(|c| c.starts_with("worktree remove"))
        );
        let task = db.get_task(id).unwrap().unwrap();
        assert_eq!(
            (task.state, task.repo.as_deref()),
            (TaskState::Queued, Some("api"))
        );
        std::fs::remove_dir_all(wt).unwrap();
    }

    #[test]
    fn retry_in_repo_refuses_without_touching_anything() {
        let (db, id, wt) = cancelled_with_worktree("rehome_refuses");
        let unchanged = |db: &StateDb| {
            let task = db.get_task(id).unwrap().unwrap();
            assert_eq!(
                (task.state, task.repo.as_deref()),
                (TaskState::Cancelled, Some("web"))
            );
        };

        let clean = FakeGit {
            status: "",
            calls: Default::default(),
        };
        let unknown = retry_in_repo(
            &db,
            id,
            "test --repo",
            "nope",
            &repos(),
            &WorktreeManager::new(&clean),
        )
        .unwrap();
        assert!(unknown.reason.unwrap().contains("`web`, `api`"));
        assert!(clean.calls.borrow().is_empty());
        unchanged(&db);

        let dirty = FakeGit {
            status: " M src/lib.rs",
            calls: Default::default(),
        };
        let kept = retry_in_repo(
            &db,
            id,
            "test --repo",
            "api",
            &repos(),
            &WorktreeManager::new(&dirty),
        )
        .unwrap();
        assert!(kept.reason.unwrap().contains("uncommitted changes"));
        assert!(
            !dirty
                .calls
                .borrow()
                .iter()
                .any(|c| c.starts_with("worktree remove"))
        );
        unchanged(&db);
        std::fs::remove_dir_all(wt).unwrap();
    }

    #[test]
    fn retry_in_repo_refuses_a_task_that_is_not_retryable_before_any_git() {
        let (db, id) = db_with_task();
        let git = FakeGit {
            status: "",
            calls: Default::default(),
        };
        let refused = retry_in_repo(
            &db,
            TaskId(id),
            "test --repo",
            "api",
            &repos(),
            &WorktreeManager::new(&git),
        )
        .unwrap();
        assert!(refused.reason.unwrap().contains("cancel` it first"));
        assert!(git.calls.borrow().is_empty());
    }

    #[test]
    fn refusals_are_answers_with_advice_not_errors() {
        let (db, id) = db_with_task();
        // Queued is not retryable.
        let refused = retry(&db, TaskId(id), control()).unwrap();
        assert!(!refused.ok);
        assert!(refused.reason.unwrap().contains("cancel` it first"));

        cancel(&db, TaskId(id), control()).unwrap();
        let twice = cancel(&db, TaskId(id), control()).unwrap();
        assert!(!twice.ok);
        assert!(twice.reason.unwrap().contains(&format!("task retry {id}")));

        let unknown = cancel(&db, TaskId(9999), control()).unwrap();
        assert_eq!(
            unknown.reason.as_deref(),
            Some(not_found(TaskId(9999)).as_str())
        );
    }
}
