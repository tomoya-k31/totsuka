//! The rules for cancelling and retrying one task (#760).
//!
//! Two callers apply them: the running engine, answering `POST /task/cancel`
//! / `POST /task/retry` on the control socket, and the CLI writing the state
//! DB directly when no engine answers. Both must refuse the same requests with
//! the same advice, so the rules live here rather than in either of them. The
//! state machine itself stays in [`transition`](crate::domain::state::transition);
//! this only decides which event to ask for and phrases the refusals.

use crate::adapters::state_db::{StateDb, StateError};
use crate::domain::EventDetail;
use crate::domain::state::{TaskEvent, TaskState};
use crate::ports::signal_ingress::TaskControlOutcome;

/// Cancel task `id`. `detail` is the audit detail recorded with the event.
///
/// Only the DB side: the engine's in-memory bookkeeping (slot, session
/// routes) is the engine's to release after an applied cancel.
pub fn cancel(
    db: &StateDb,
    id: i64,
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
pub fn retry(db: &StateDb, id: i64, detail: EventDetail) -> Result<TaskControlOutcome, StateError> {
    let Some(task) = db.get_task(id)? else {
        return Ok(TaskControlOutcome::refused(not_found(id)));
    };
    if !matches!(
        task.state,
        TaskState::Failed | TaskState::Cancelled | TaskState::Skipped
    ) {
        let action = if task.state == TaskState::Done {
            // Since #242 `done` means "no unprocessed messages", not "closed
            // forever": a new message reopens the conversation. Re-running the
            // same instructions is a different thing, and not what anyone
            // asking about a finished task wants.
            "it finished; send another message in the conversation (the reply in its thread/issue) to continue it — a re-run of the same instructions is not what `retry` is for"
        } else {
            "only failed/cancelled/skipped tasks can be retried; `totsuka task cancel` it first if you want a re-run"
        };
        return Ok(TaskControlOutcome::refused(format!(
            "task {id} is {} → {action}",
            task.state
        )));
    }
    // `retry_task`, not `apply_event(Retry)`: requeueing the task without the
    // messages its failed run was given would dispatch an empty prompt (#242).
    let (to, requeued) = match db.retry_task(task.task_ref(), Some(detail)) {
        Ok((to, _, requeued)) => (to, requeued),
        Err(e @ (StateError::Conflict(_) | StateError::Transition(_))) => {
            return Ok(lost_race(id, &e));
        }
        Err(e) => return Err(e),
    };
    Ok(TaskControlOutcome::applied(task.state, to, Some(requeued)))
}

/// The state moved between the check above and the write — another writer
/// (the CLI writing the DB directly, until #760's CLI switch-over) got there
/// first. Since #763 that arrives as [`StateError::Conflict`]; `Transition`
/// can no longer mean a race here, only a refusal the checks above missed,
/// and is answered the same way rather than stopping `run`. A refusal, not an error: inside the engine an `Err` is run-fatal,
/// and losing this race must not stop `run`.
fn lost_race(id: i64, e: &impl std::fmt::Display) -> TaskControlOutcome {
    TaskControlOutcome::refused(format!(
        "task {id} changed state while this was being applied ({e}) → `totsuka task show {id}` and try again"
    ))
}

/// The refusal for an id the DB does not know.
pub fn not_found(id: i64) -> String {
    format!("task {id} not found → `totsuka task list` shows known ids")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::state_db::NewTask;

    fn db_with_task() -> (StateDb, i64) {
        let db = StateDb::open_in_memory().unwrap();
        let id = db
            .upsert_task(&NewTask {
                source: "github".to_string(),
                source_task_id: "42".to_string(),
                workflow: "implement".to_string(),
                mode: "implement".to_string(),
                repo: Some("web".to_string()),
                priority: 0,
                title: "Fix the bug".to_string(),
                url: None,
                source_payload: None,
                last_signal_at: None,
            })
            .unwrap();
        (db, id)
    }

    fn control() -> EventDetail {
        EventDetail::Control {
            command: "test".to_string(),
        }
    }

    #[test]
    fn cancel_then_retry_round_trips_and_reports_where_it_came_from() {
        let (db, id) = db_with_task();
        let cancelled = cancel(&db, id, control()).unwrap();
        assert_eq!(
            cancelled,
            TaskControlOutcome::applied(TaskState::Queued, TaskState::Cancelled, None)
        );
        let retried = retry(&db, id, control()).unwrap();
        assert_eq!(
            retried,
            TaskControlOutcome::applied(TaskState::Cancelled, TaskState::Queued, Some(0))
        );
    }

    #[test]
    fn refusals_are_answers_with_advice_not_errors() {
        let (db, id) = db_with_task();
        // Queued is not retryable.
        let refused = retry(&db, id, control()).unwrap();
        assert!(!refused.ok);
        assert!(refused.reason.unwrap().contains("cancel` it first"));

        cancel(&db, id, control()).unwrap();
        let twice = cancel(&db, id, control()).unwrap();
        assert!(!twice.ok);
        assert!(twice.reason.unwrap().contains(&format!("task retry {id}")));

        let unknown = cancel(&db, 9999, control()).unwrap();
        assert_eq!(unknown.reason.as_deref(), Some(not_found(9999).as_str()));
    }
}
