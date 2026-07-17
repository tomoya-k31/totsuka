//! Restart recovery: re-attach to in-flight agent sessions (F-37, F-44, §5.3).
//!
//! When the Orchestrator restarts, tasks that were mid-flight still have live
//! (or dead) agent sessions. [`recover`] walks the tasks that hold a session,
//! re-attaches to each via the [`AgentSession`] port, and reconciles the
//! persisted state to what the agent reports:
//!
//! - **Attached** → sync the state machine forward to the agent's state and
//!   resume (the port also re-establishes the `state/subscribe` stream).
//! - **Session lost / attach error / no session / agent failed** → the task is
//!   handed to a human as *needs confirmation*. It is **never auto-failed**
//!   (§5.3); the human chooses `task retry` or `task cancel` ([`NEXT_ACTIONS`]).
//!
//! Retry reuse (F-44) is decided by [`retry_plan`]; slot re-acquisition (#55) is
//! seeded by [`active_slot_claims`]. Both are pure/DB-only so the run loop (#63)
//! can drive them.

use plugin_protocol::methods::AgentState;

use crate::adapters::state_db::{SessionRecord, StateDb, StateError, TaskRecord};
use crate::domain::state::{TaskEvent, TaskState};
use crate::ports::agent_session::{AgentSession, AttachOutcome};
use crate::scheduler::counts_toward_slot;

/// Task states that hold an in-flight agent session and must be re-attached on
/// startup (§5.3).
pub const RECOVERABLE_STATES: [TaskState; 6] = [
    TaskState::Dispatched,
    TaskState::Running,
    TaskState::WaitingInput,
    TaskState::Verifying,
    TaskState::Escalated,
    TaskState::Publishing,
];

/// The next actions offered to a human for a task that could not be recovered
/// automatically (§5.3). Surfaced by `status` (#64).
pub const NEXT_ACTIONS: [&str; 2] = ["task retry <id>", "task cancel <id>"];

/// What happened to one task during recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryResult {
    /// Re-attached and the state machine synced to `state`; work resumes.
    Resumed {
        /// The task's state after syncing to the agent.
        state: TaskState,
    },
    /// Could not resume automatically (session lost, attach error, no session,
    /// or the agent itself failed). The task is **not** failed; a human decides
    /// via [`NEXT_ACTIONS`].
    NeedsConfirmation {
        /// Why automatic recovery did not resume the task.
        reason: String,
    },
}

/// The recovery outcome for a single task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecovery {
    /// Task id.
    pub task_id: i64,
    /// Owning plugin, if a session was recorded.
    pub plugin: Option<String>,
    /// Session id that was re-attached (or attempted), if any.
    pub session_id: Option<String>,
    /// The result.
    pub result: RecoveryResult,
}

impl TaskRecovery {
    /// Whether the task resumed.
    pub fn is_resumed(&self) -> bool {
        matches!(self.result, RecoveryResult::Resumed { .. })
    }

    /// Whether the task needs human confirmation.
    pub fn is_needs_confirmation(&self) -> bool {
        matches!(self.result, RecoveryResult::NeedsConfirmation { .. })
    }
}

/// The result of a whole recovery pass (one entry per recoverable task).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Per-task outcomes, in the order they were processed.
    pub outcomes: Vec<TaskRecovery>,
}

impl RecoveryReport {
    /// Tasks that resumed.
    pub fn resumed(&self) -> impl Iterator<Item = &TaskRecovery> {
        self.outcomes.iter().filter(|o| o.is_resumed())
    }

    /// Tasks awaiting a human decision.
    pub fn needs_confirmation(&self) -> impl Iterator<Item = &TaskRecovery> {
        self.outcomes.iter().filter(|o| o.is_needs_confirmation())
    }
}

/// Re-attach to every in-flight task's session and reconcile its state (§5.3).
///
/// State-machine syncing is persisted as it happens (via `apply_event`), so the
/// report reflects the DB. Any I/O error against the state DB aborts the pass.
pub async fn recover<A: AgentSession>(
    db: &StateDb,
    attacher: &A,
) -> Result<RecoveryReport, StateError> {
    // Snapshot the whole recoverable set up front. Each task is in exactly one
    // state, so this is a set of distinct tasks; snapshotting before we touch
    // any of them means a task synced *forward* (e.g. Dispatched → Running) is
    // not re-selected — and re-attached — when a later state is processed.
    let mut tasks = Vec::new();
    for state in RECOVERABLE_STATES {
        tasks.extend(db.tasks_in_state(state)?);
    }
    let mut outcomes = Vec::with_capacity(tasks.len());
    for task in &tasks {
        outcomes.push(recover_task(db, attacher, task).await?);
    }
    Ok(RecoveryReport { outcomes })
}

/// Recover a single task: look up its latest session, attach, reconcile.
async fn recover_task<A: AgentSession>(
    db: &StateDb,
    attacher: &A,
    task: &TaskRecord,
) -> Result<TaskRecovery, StateError> {
    let Some(session) = db.latest_session(task.id)? else {
        // Dispatched (or further) with no recorded session: nothing to attach
        // to. Hand to the human rather than guess (§5.3).
        return Ok(needs_confirmation(
            task.id,
            None,
            None,
            "no persisted session to re-attach to".to_string(),
        ));
    };
    let plugin = Some(session.plugin.clone());
    let sid = Some(session.session_id.clone());

    match attacher.attach(&session.plugin, &session.session_id).await {
        Ok(AttachOutcome::Attached(agent)) => match resume_plan(task.state, agent) {
            ResumeDecision::Synced => Ok(resumed(task.id, plugin, sid, task.state)),
            ResumeDecision::Apply(events) => {
                let detail = recovery_detail(agent);
                let mut state = task.state;
                for &event in events {
                    state = db.apply_event(task.id, event, Some(detail.clone()))?;
                }
                Ok(resumed(task.id, plugin, sid, state))
            }
            ResumeDecision::Confirm(reason) => {
                Ok(needs_confirmation(task.id, plugin, sid, reason.to_string()))
            }
        },
        Ok(AttachOutcome::Lost) => Ok(needs_confirmation(
            task.id,
            plugin,
            sid,
            "the agent session no longer exists on the plugin".to_string(),
        )),
        Err(e) => Ok(needs_confirmation(task.id, plugin, sid, e.to_string())),
    }
}

/// How to reconcile a persisted state with the agent's reported state.
enum ResumeDecision {
    /// Already consistent; apply nothing.
    Synced,
    /// Apply these events in order to sync forward, then resume. Every sequence
    /// is a legal state-machine path (asserted in tests).
    Apply(&'static [TaskEvent]),
    /// Ambiguous / regressed / agent-failed: hand to a human, do not auto-fail.
    Confirm(&'static str),
}

/// Decide how to sync a recoverable task's state to the agent's state (F-32).
fn resume_plan(current: TaskState, agent: AgentState) -> ResumeDecision {
    use AgentState as A;
    use TaskEvent as E;
    use TaskState as S;

    // Human-gated states never auto-resume, whatever the agent reports:
    // verification / escalation must not be skipped across a restart (#133).
    if current == S::Verifying {
        return ResumeDecision::Confirm(
            "task awaits human verification (`totsuka task verify`); not resumed automatically",
        );
    }
    if current == S::Escalated {
        return ResumeDecision::Confirm(
            "task was escalated to a human; resolve it before resuming",
        );
    }

    match agent {
        // A successfully-attached agent that reports failure is a real failure,
        // but recovery still defers to the human (§5.3: never auto-fail here).
        A::Failed => ResumeDecision::Confirm("agent reported it had failed after re-attach"),
        A::Idle => match current {
            S::Dispatched => ResumeDecision::Synced, // dispatched, not yet started
            _ => ResumeDecision::Confirm("agent is idle but the task had already started"),
        },
        A::Running => match current {
            S::Dispatched => ResumeDecision::Apply(&[E::Start]),
            S::Running => ResumeDecision::Synced,
            S::WaitingInput => ResumeDecision::Apply(&[E::ResumeInput]),
            _ => ResumeDecision::Confirm("agent is running but the task had moved to publishing"),
        },
        A::WaitingInput => match current {
            S::Dispatched => ResumeDecision::Apply(&[E::Start, E::WaitInput]),
            S::Running => ResumeDecision::Apply(&[E::WaitInput]),
            S::WaitingInput => ResumeDecision::Synced,
            _ => ResumeDecision::Confirm(
                "agent is waiting for input but the task had moved to publishing",
            ),
        },
        A::Done => match current {
            S::Dispatched => ResumeDecision::Apply(&[E::Start, E::BeginPublish]),
            S::Running => ResumeDecision::Apply(&[E::BeginPublish]),
            // A waiting task whose agent finished may be a human-verification
            // outcome in disguise: auto-publishing here would skip the review
            // across a restart, so defer to the human (#133 safety).
            S::WaitingInput => ResumeDecision::Confirm(
                "agent finished while the task was waiting for input; verify before publishing",
            ),
            S::Publishing => ResumeDecision::Synced,
            _ => ResumeDecision::Confirm("agent is done but the task was in an unexpected state"),
        },
    }
}

/// How to re-run a retried task (F-44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryPlan {
    /// A worktree and session survive: reuse the worktree and re-attach to the
    /// existing session to resume the conversation (F-44).
    ReuseSession {
        /// Existing worktree path.
        worktree_path: String,
        /// Existing branch.
        branch: String,
        /// Owning plugin.
        plugin: String,
        /// Session id to re-attach to.
        session_id: String,
    },
    /// No reusable worktree/session: create a fresh worktree and dispatch anew
    /// (appending a new session to the history).
    FreshDispatch,
}

/// Decide how to retry `task` given its most recent session (F-44).
///
/// Reuse requires both a recorded worktree (path + branch, #53) and a session;
/// anything missing means a clean re-dispatch.
pub fn retry_plan(task: &TaskRecord, latest_session: Option<&SessionRecord>) -> RetryPlan {
    match (
        task.worktree_path.as_ref(),
        task.branch.as_ref(),
        latest_session,
    ) {
        (Some(worktree_path), Some(branch), Some(session)) => RetryPlan::ReuseSession {
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            plugin: session.plugin.clone(),
            session_id: session.session_id.clone(),
        },
        _ => RetryPlan::FreshDispatch,
    }
}

/// The `(repo, agent)` slot claims to feed [`SlotManager::rebuild`] after a
/// restart (#55), derived from a [`recover`] pass.
///
/// Only tasks that actually **resumed** into a slot-counting state hold a slot:
/// a task awaiting human confirmation (`session/attach` failed, §5.3) is paused,
/// so it must not occupy an agent slot — it re-acquires one through normal
/// dispatch when the human retries it. Because a resumed task always has a
/// re-attached session, its owning plugin and repository are both known, so no
/// claim is ever misattributed to an empty agent/repo bucket.
///
/// [`SlotManager::rebuild`]: crate::scheduler::SlotManager::rebuild
pub fn active_slot_claims(
    db: &StateDb,
    report: &RecoveryReport,
) -> Result<Vec<(String, String)>, StateError> {
    let mut claims = Vec::new();
    for recovery in report.resumed() {
        let RecoveryResult::Resumed { state } = recovery.result else {
            continue;
        };
        // `WaitingInput` frees its slot (F-45); `counts_toward_slot` excludes it.
        if !counts_toward_slot(state) {
            continue;
        }
        // Resumed ⇒ re-attached to a session ⇒ owning plugin is known.
        let Some(agent) = recovery.plugin.clone() else {
            continue;
        };
        // A dispatched (hence resumable) task always has its repo selected.
        let task = db
            .get_task(recovery.task_id)?
            .ok_or(StateError::NotFound(recovery.task_id))?;
        let Some(repo) = task.repo else {
            continue;
        };
        claims.push((repo, agent));
    }
    Ok(claims)
}

/// The `events.detail` recorded for a recovery-driven transition.
fn recovery_detail(agent: AgentState) -> serde_json::Value {
    serde_json::json!({ "kind": "recovery", "attached": true, "agent_state": agent })
}

/// Build a `Resumed` outcome.
fn resumed(
    task_id: i64,
    plugin: Option<String>,
    session_id: Option<String>,
    state: TaskState,
) -> TaskRecovery {
    TaskRecovery {
        task_id,
        plugin,
        session_id,
        result: RecoveryResult::Resumed { state },
    }
}

/// Build a `NeedsConfirmation` outcome.
fn needs_confirmation(
    task_id: i64,
    plugin: Option<String>,
    session_id: Option<String>,
    reason: String,
) -> TaskRecovery {
    TaskRecovery {
        task_id,
        plugin,
        session_id,
        result: RecoveryResult::NeedsConfirmation { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::state_db::NewTask;
    use crate::ports::agent_session::AgentSessionError;
    use std::collections::HashMap;
    use std::future::Future;

    /// A canned re-attach response (kept `Clone` since `AgentSessionError` is not).
    #[derive(Clone)]
    enum Canned {
        Attached(AgentState),
        Lost,
        Fail,
    }

    /// A fake attacher returning canned responses keyed by session id.
    struct FakeAttacher {
        by_session: HashMap<String, Canned>,
    }

    impl FakeAttacher {
        fn new(entries: &[(&str, Canned)]) -> Self {
            Self {
                by_session: entries
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
            }
        }
    }

    impl AgentSession for FakeAttacher {
        fn attach(
            &self,
            _plugin: &str,
            session_id: &str,
        ) -> impl Future<Output = Result<AttachOutcome, AgentSessionError>> + Send {
            let canned = self.by_session.get(session_id).cloned();
            let session_id = session_id.to_string();
            async move {
                match canned {
                    Some(Canned::Attached(state)) => Ok(AttachOutcome::Attached(state)),
                    Some(Canned::Lost) => Ok(AttachOutcome::Lost),
                    Some(Canned::Fail) | None => Err(AgentSessionError::Attach {
                        plugin: "fake".to_string(),
                        reason: format!("no route for {session_id}"),
                    }),
                }
            }
        }
    }

    fn new_task(source_task_id: &str) -> NewTask {
        NewTask {
            source: "github".into(),
            source_task_id: source_task_id.into(),
            workflow: "implement".into(),
            mode: "implement".into(),
            repo: Some("totsuka".into()),
            priority: 0,
            title: "t".into(),
            url: None,
            source_payload: None,
        }
    }

    /// Ingest a task and drive it to `state`, recording a session if given.
    fn task_in(db: &StateDb, source_task_id: &str, state: TaskState, session: Option<&str>) -> i64 {
        let id = db.upsert_task(&new_task(source_task_id)).unwrap();
        // Each state is reached along the linear pipeline; Publishing branches
        // from Running (not through WaitingInput).
        let events: &[TaskEvent] = match state {
            TaskState::Dispatched => &[TaskEvent::Dispatch],
            TaskState::Running => &[TaskEvent::Dispatch, TaskEvent::Start],
            TaskState::WaitingInput => {
                &[TaskEvent::Dispatch, TaskEvent::Start, TaskEvent::WaitInput]
            }
            TaskState::Verifying => &[
                TaskEvent::Dispatch,
                TaskEvent::Start,
                TaskEvent::SelfReportComplete,
            ],
            TaskState::Escalated => &[TaskEvent::Dispatch, TaskEvent::Start, TaskEvent::Escalate],
            TaskState::Publishing => &[
                TaskEvent::Dispatch,
                TaskEvent::Start,
                TaskEvent::BeginPublish,
            ],
            other => panic!("unsupported test state {other}"),
        };
        for &event in events {
            db.apply_event(id, event, None).unwrap();
        }
        if let Some(sid) = session {
            db.record_session(id, "herdr", sid).unwrap();
        }
        id
    }

    #[tokio::test]
    async fn attached_running_task_resumes() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Attached(AgentState::Running))]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.resumed().count(), 1);
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
    }

    #[tokio::test]
    async fn dispatched_task_with_running_agent_starts() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Dispatched, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Attached(AgentState::Running))]);

        let report = recover(&db, &attacher).await.unwrap();

        // Synced forward Dispatched -> Running. The task is processed exactly
        // once even though it is advanced into another recoverable state
        // (regression guard: a forward sync must not re-select the task).
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
        assert!(matches!(
            report.outcomes[0].result,
            RecoveryResult::Resumed {
                state: TaskState::Running
            }
        ));
    }

    #[tokio::test]
    async fn running_task_syncs_to_waiting_input() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Attached(AgentState::WaitingInput))]);

        recover(&db, &attacher).await.unwrap();

        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::WaitingInput
        );
    }

    #[tokio::test]
    async fn lost_session_needs_confirmation_not_failed() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, Some("sess-gone"));
        let attacher = FakeAttacher::new(&[("sess-gone", Canned::Lost)]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 1);
        // Crucially: not auto-failed (§5.3).
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
        assert!(!NEXT_ACTIONS.is_empty(), "human is offered next actions");
    }

    #[tokio::test]
    async fn attach_error_needs_confirmation_not_failed() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Fail)]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 1);
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
    }

    #[tokio::test]
    async fn no_session_needs_confirmation() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, None); // never recorded a session
        let attacher = FakeAttacher::new(&[]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 1);
        assert_eq!(report.outcomes[0].session_id, None);
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
    }

    #[tokio::test]
    async fn agent_failed_needs_confirmation_not_failed() {
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::Running, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Attached(AgentState::Failed))]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 1);
        // Attach succeeded but the agent failed: still defer to the human.
        assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);
    }

    #[tokio::test]
    async fn done_agent_with_waiting_task_needs_confirmation_not_auto_publish() {
        // Regression guard (#133 safety): a task that was waiting for input
        // when the agent finished may be pending human verification — a
        // restart must not skip the review by auto-publishing.
        let db = StateDb::open_in_memory().unwrap();
        let id = task_in(&db, "1", TaskState::WaitingInput, Some("sess-1"));
        let attacher = FakeAttacher::new(&[("sess-1", Canned::Attached(AgentState::Done))]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 1);
        assert_eq!(
            db.get_task(id).unwrap().unwrap().state,
            TaskState::WaitingInput,
            "must not advance to Publishing without a human"
        );
    }

    #[tokio::test]
    async fn verifying_and_escalated_tasks_are_recovered_but_deferred() {
        // Both states are in RECOVERABLE_STATES (they hold live sessions) but
        // never auto-resume, whatever the agent reports.
        let db = StateDb::open_in_memory().unwrap();
        let v = task_in(&db, "1", TaskState::Verifying, Some("sess-v"));
        let e = task_in(&db, "2", TaskState::Escalated, Some("sess-e"));
        let attacher = FakeAttacher::new(&[
            ("sess-v", Canned::Attached(AgentState::Done)),
            ("sess-e", Canned::Attached(AgentState::Running)),
        ]);

        let report = recover(&db, &attacher).await.unwrap();

        assert_eq!(report.needs_confirmation().count(), 2);
        assert_eq!(db.get_task(v).unwrap().unwrap().state, TaskState::Verifying);
        assert_eq!(db.get_task(e).unwrap().unwrap().state, TaskState::Escalated);
    }

    #[test]
    fn human_gated_states_always_confirm_in_resume_plan() {
        for current in [TaskState::Verifying, TaskState::Escalated] {
            for agent in [
                AgentState::Idle,
                AgentState::Running,
                AgentState::WaitingInput,
                AgentState::Done,
                AgentState::Failed,
            ] {
                assert!(
                    matches!(resume_plan(current, agent), ResumeDecision::Confirm(_)),
                    "{current} + {agent:?} must defer to a human"
                );
            }
        }
    }

    #[test]
    fn every_resume_plan_path_is_a_legal_transition() {
        use crate::domain::state::transition;
        for current in RECOVERABLE_STATES {
            for agent in [
                AgentState::Idle,
                AgentState::Running,
                AgentState::WaitingInput,
                AgentState::Done,
                AgentState::Failed,
            ] {
                if let ResumeDecision::Apply(events) = resume_plan(current, agent) {
                    let mut state = current;
                    for &event in events {
                        state = transition(state, event)
                            .unwrap_or_else(|e| panic!("{current} + {agent:?}: illegal step: {e}"));
                    }
                }
            }
        }
    }

    #[test]
    fn retry_plan_reuses_when_worktree_and_session_present() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&new_task("1")).unwrap();
        db.set_worktree(id, "/wt/agent-github-1", "agent/github-1")
            .unwrap();
        db.record_session(id, "herdr", "sess-1").unwrap();
        let task = db.get_task(id).unwrap().unwrap();
        let session = db.latest_session(id).unwrap();

        assert_eq!(
            retry_plan(&task, session.as_ref()),
            RetryPlan::ReuseSession {
                worktree_path: "/wt/agent-github-1".into(),
                branch: "agent/github-1".into(),
                plugin: "herdr".into(),
                session_id: "sess-1".into(),
            }
        );
    }

    #[test]
    fn retry_plan_fresh_without_worktree_or_session() {
        let db = StateDb::open_in_memory().unwrap();
        let id = db.upsert_task(&new_task("1")).unwrap();
        let task = db.get_task(id).unwrap().unwrap();

        // No worktree, no session.
        assert_eq!(retry_plan(&task, None), RetryPlan::FreshDispatch);
        // Session but no worktree -> still fresh.
        db.record_session(id, "herdr", "sess-1").unwrap();
        let session = db.latest_session(id).unwrap();
        assert_eq!(
            retry_plan(&task, session.as_ref()),
            RetryPlan::FreshDispatch
        );
    }

    #[tokio::test]
    async fn active_slot_claims_counts_resumed_slot_holders_only() {
        let db = StateDb::open_in_memory().unwrap();
        task_in(&db, "1", TaskState::Running, Some("sess-1"));
        task_in(&db, "2", TaskState::Publishing, Some("sess-2"));
        task_in(&db, "3", TaskState::WaitingInput, Some("sess-3")); // frees its slot
        task_in(&db, "4", TaskState::Dispatched, Some("sess-4"));
        // Agent states that resume each task into its slot-holding state.
        let attacher = FakeAttacher::new(&[
            ("sess-1", Canned::Attached(AgentState::Running)),
            ("sess-2", Canned::Attached(AgentState::Done)), // stays Publishing
            ("sess-3", Canned::Attached(AgentState::WaitingInput)),
            ("sess-4", Canned::Attached(AgentState::Running)), // Dispatched -> Running
        ]);

        let report = recover(&db, &attacher).await.unwrap();
        let mut claims = active_slot_claims(&db, &report).unwrap();
        claims.sort();

        // Running, Publishing, Dispatched->Running hold slots; WaitingInput does not.
        assert_eq!(
            claims,
            vec![
                ("totsuka".to_string(), "herdr".to_string()),
                ("totsuka".to_string(), "herdr".to_string()),
                ("totsuka".to_string(), "herdr".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn active_slot_claims_skips_tasks_needing_confirmation() {
        let db = StateDb::open_in_memory().unwrap();
        task_in(&db, "1", TaskState::Running, Some("sess-1")); // resumes -> holds a slot
        task_in(&db, "2", TaskState::Running, Some("sess-gone")); // lost -> paused
        task_in(&db, "3", TaskState::Running, None); // no session -> paused
        let attacher = FakeAttacher::new(&[
            ("sess-1", Canned::Attached(AgentState::Running)),
            ("sess-gone", Canned::Lost),
        ]);

        let report = recover(&db, &attacher).await.unwrap();
        let claims = active_slot_claims(&db, &report).unwrap();

        // Only the resumed task holds a slot; paused tasks awaiting a human do
        // not, and never contribute an empty-agent claim.
        assert_eq!(claims, vec![("totsuka".to_string(), "herdr".to_string())]);
    }
}
