//! What happens after an agent reports it is done (#464).
//!
//! The output policy (F-07), the `on_success`/`on_failure` write-back (F-84),
//! and worktree cleanup (F-23/F-85). A publishing failure fails the task but
//! keeps its worktree and commits, so `task retry` can resume from here.

use super::*;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Terminal processing for a task whose agent finished: run the workflow's
    /// output policy (#65), then either complete or fail.
    ///
    /// `record` is in a pre-`Complete` pipeline state (usually `Publishing`).
    /// On a **publishing failure** the task is failed but its worktree and
    /// commits are kept, so `task retry` can resume from here (issue #65).
    pub(super) async fn finalize_success(
        &mut self,
        record: &TaskRecord,
    ) -> Result<(), EngineError> {
        // Last chance to learn the branch before anything consumes it. The
        // Stop handler already syncs, but only hook-capable agents send a
        // Stop — a non-hook agent (orca, the mock) reports completion through
        // `state/notification` and reaches here having produced no signal at
        // all. Re-reading is cheap and idempotent.
        self.sync_branch(record.id)?;
        let record = &self
            .db
            .get_task(record.id)?
            .unwrap_or_else(|| record.clone());
        // A read-only profile that ended up on a branch touched the repository
        // it was told not to touch (#409/#410). Refuse to publish it as a
        // success: `fail_publish` keeps the worktree and the commits, so a
        // human can see what happened, and the notifier says so out loud.
        //
        // This does not *prevent* anything — by the time a branch exists the
        // agent may already have pushed it — but "loudly failed with the
        // evidence retained" is a different operational state from "reported
        // done", and the live #410 run produced the second one.
        //
        // One lookup serves both this check and the output policy below. The
        // map borrows `self.settings`, and everything after it needs
        // `&mut self`, so the two values are copied out first.
        let resolved = workflows_by_name(&self.settings.workflows)
            .get(record.workflow.as_str())
            .map(|w| (w.profile, w.output));
        //
        // The branch is read from the worktree's **live `HEAD`**, not from
        // `record.branch`. The recorded column is write-once — `sync_branch`
        // deliberately leaves a detached head unrecorded rather than clearing
        // it, and `retry_task` does not touch it — so gating on it would make
        // this failure unrecoverable: every `totsuka task retry` would reach
        // here, read the same stale value, and fail again forever. Reading
        // `HEAD` makes the check describe the worktree as it is now, so
        // detaching it is a remedy the operator can actually apply (and one
        // the failure message names).
        let live_branch = record
            .worktree_path
            .as_deref()
            .filter(|p| Path::new(p).is_dir())
            .and_then(|p| self.worktrees.head_branch(Path::new(p)));
        if let Some(reason) = read_only_side_effect(
            &record.workflow,
            resolved.and_then(|(profile, _)| profile),
            live_branch.as_deref(),
            record.id,
            record.worktree_path.as_deref().unwrap_or("<worktree>"),
        ) {
            return self
                .fail_publish(record, "read_only_violation", reason)
                .await;
        }
        // A finished task whose workflow vanished from config still holds the
        // agent's commits; treat it as a recoverable publish failure rather
        // than silently completing and deleting the worktree (never confuse a
        // missing workflow with an explicit `output = none`).
        let Some((_, policy)) = resolved else {
            return self
                .fail_publish(
                    record,
                    "publish",
                    format!(
                        "workflow `{}` is no longer configured → restore it (worktree and commits are kept) or `totsuka task cancel {}`",
                        record.workflow, record.id
                    ),
                )
                .await;
        };

        match self.execute_output_policy(record, policy).await {
            Ok(pr_url) => {
                // Success: on_success write-back (F-84) → Complete → cleanup.
                self.write_back_status(record, true).await;
                self.db.apply_event(
                    record.id,
                    TaskEvent::Complete,
                    Some(serde_json::json!({
                        "kind": "publish",
                        "policy": policy_str(policy),
                        "pr_url": pr_url,
                    })),
                )?;
                self.release_slot(record.id);
                self.drop_task_sessions(record.id);
                self.agent_output.remove(&record.id);
                self.stats.done += 1;
                self.cleanup_worktree(record.id).await?;
                notify_all(&self.plugins.notifiers, NotifierEvent::Done, record, None);
                tracing::info!(task_id = record.id, "task done");
                Ok(())
            }
            Err(reason) => self.fail_publish(record, "publish", reason).await,
        }
    }

    /// Fail a task recoverably, KEEPING its worktree, commits and session so
    /// `task retry` can resume (issue #65). The accumulated agent output is
    /// dropped so a retry re-captures fresh output (no duplication in the
    /// publish artifact). The source status is intentionally left unchanged: a
    /// recoverable failure must not flap the source task to `on_failure` and
    /// back on the next successful retry.
    ///
    /// `kind` names *which* check refused, and it is not decoration: it is the
    /// audit `detail.kind` **and** the log line. Named `fail_publish` because
    /// publishing was the only caller at first; since
    /// [`enforce_read_only`](Self::enforce_read_only) it is not, and a log line
    /// hardcoded to "output policy failed" was reporting a mid-run violation as
    /// a publish failure — the output policy had not run at all (#410).
    pub(super) async fn fail_publish(
        &mut self,
        record: &TaskRecord,
        kind: &str,
        reason: String,
    ) -> Result<(), EngineError> {
        tracing::error!(task_id = record.id, kind, "task failed: {reason}");
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": kind, "reason": reason.clone() })),
        )?;
        self.release_slot(record.id);
        self.agent_output.remove(&record.id);
        self.stats.failed += 1;
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Failed,
            record,
            Some(reason),
        );
        Ok(())
    }

    /// Execute the output policy for a finished task, or an error reason on
    /// failure.
    async fn execute_output_policy(
        &self,
        record: &TaskRecord,
        policy: OutputPolicy,
    ) -> Result<Option<String>, String> {
        match policy {
            OutputPolicy::None => Ok(None),
            OutputPolicy::Source => self.publish_to_source(record).await.map(|()| None),
        }
    }

    /// The agent artifact persisted on the most recent `BeginPublish`
    /// transition (the `publish_artifact` field of its event `detail`), if any.
    /// Used to recover the artifact across a restart.
    pub(super) fn persisted_artifact(&self, task_id: i64) -> Result<Option<String>, EngineError> {
        Ok(self
            .db
            .list_events(task_id)?
            .into_iter()
            .rev()
            .find_map(|e| {
                e.detail
                    .as_ref()
                    .and_then(|d| d.get("publish_artifact"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            }))
    }

    /// `output = source` (F-07): hand the accumulated artifact to the task
    /// source plugin's `result/publish`.
    async fn publish_to_source(&self, record: &TaskRecord) -> Result<(), String> {
        let source = self
            .plugins
            .sources
            .get(&record.source)
            .ok_or_else(|| format!("task source `{}` is not launched", record.source))?;
        // The accumulated agent output is the artifact. When it is genuinely
        // unavailable — an agent that finished while the orchestrator was fully
        // down streamed nothing to anyone — publish an honest note rather than
        // pretend a result exists.
        let content = self
            .agent_output
            .get(&record.id)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "_totsuka: task `{}` completed, but the agent output was not captured (recovered run)._",
                    record.title
                )
            });
        let params = ResultPublishParams {
            task_id: record.source_task_id.clone(),
            content,
            format: Some("markdown".to_string()),
        };
        source
            .call::<_, Value>(method::RESULT_PUBLISH, &params)
            .await
            .map(|_| ())
            .map_err(|e| format!("result/publish failed: {e}"))
    }

    /// Apply the workflow's `on_success`/`on_failure` status transition on the
    /// source (F-84). Failures are logged, never fatal: the task outcome is
    /// already decided.
    pub(super) async fn write_back_status(&self, record: &TaskRecord, success: bool) {
        let workflows = workflows_by_name(&self.settings.workflows);
        let Some(wf) = workflows.get(record.workflow.as_str()) else {
            return;
        };
        let action = if success {
            wf.on_success.as_ref()
        } else {
            wf.on_failure.as_ref()
        };
        let Some(status) = action.and_then(|a| a.set_status.clone()) else {
            return;
        };
        let Some(source) = self.plugins.sources.get(&record.source) else {
            tracing::warn!(
                task_id = record.id,
                "cannot write back status: source plugin not launched"
            );
            return;
        };
        let params = TaskUpdateStatusParams {
            task_id: record.source_task_id.clone(),
            status: status.clone(),
        };
        match source
            .call::<_, Value>(method::TASK_UPDATE_STATUS, &params)
            .await
        {
            Ok(_) => {
                tracing::info!(task_id = record.id, status = %status, "source status updated (F-84)");
            }
            Err(e) => {
                tracing::warn!(task_id = record.id, "task/update_status failed: {e}");
            }
        }
    }

    /// Apply the cleanup policy to a finished task's worktree, in three stages
    /// (#210): decide → release the pane → remove. The pane is released only
    /// on a `Remove` decision, so `Retained`/`DirtySkipped` worktrees keep
    /// their pane as the human's entry point (F-23/F-85).
    pub(super) async fn cleanup_worktree(&mut self, task_id: i64) -> Result<(), EngineError> {
        // The other consumer of the branch, and the one reached by paths that
        // never publish at all (a cancel, a sweep of a task finished by an
        // earlier process). Deleting the branch is the only thing that needs
        // it, and it is the thing that silently does not happen otherwise.
        self.sync_branch(task_id)?;
        // Re-fetch: `finished_at` was just set by the terminal transition.
        let Some(record) = self.db.get_task(task_id)? else {
            return Ok(());
        };
        // A missing branch is not a reason to skip the cleanup — a worktree
        // can legitimately never have been put on one, and treating that as
        // "nothing to do" would leak the directory *and* its pane, silently,
        // on every sweep for the lifetime of the process. Only the branch
        // deletion is conditional on it; `remove` takes the `Option`.
        let (Some(path), Some(repo_name)) = (&record.worktree_path, &record.repo) else {
            return Ok(());
        };
        let branch = record.branch.as_deref();
        let base_commit = record.base_commit.as_deref();
        // Already removed (earlier run / manual cleanup): nothing to do. The
        // task will never be swept again, so drop its release memos too.
        if !Path::new(path).exists() {
            self.forget_release_memos(task_id);
            return Ok(());
        }
        // Owned copy: `release_pane` below needs `&mut self`, which a borrow
        // into `self.settings` would block.
        let Some(repo_path) = self
            .settings
            .repos
            .iter()
            .find(|r| &r.name == repo_name)
            .map(|r| r.path.clone())
        else {
            return Ok(());
        };
        let policy = if record.mode == "plan" {
            self.settings.cleanup_plan
        } else {
            self.settings.cleanup_implement
        };
        let now = self.clock.now_rfc3339();
        let decision = match self.worktrees.decide_cleanup(
            Path::new(path),
            base_commit,
            policy,
            record.finished_at.as_deref(),
            &now,
        ) {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(task_id, "worktree cleanup failed: {e}");
                return Ok(());
            }
        };
        match decision {
            CleanupDecision::Retain => {
                // Expected under retention/manual policies; the sweep re-checks
                // periodically, so keep this quiet.
                tracing::debug!(task_id, "worktree retained per policy");
                return Ok(());
            }
            CleanupDecision::Dirty => {
                // Data-loss guard (F-23): keep the worktree AND its pane — the
                // pane is the human's way in to the uncommitted work.
                tracing::info!(
                    task_id,
                    outcome = ?CleanupOutcome::DirtySkipped,
                    "worktree cleanup"
                );
                return Ok(());
            }
            CleanupDecision::Remove => {}
        }
        // The worktree is going away → its pane has nothing left to show.
        // Close it before the removal so the pane's lifetime tracks the
        // worktree's. `Once`: a removal that keeps failing must not re-release
        // on every sweep (#210).
        self.release_pane(&record, ReleaseMode::Once).await;
        match self
            .worktrees
            .remove(&repo_path, Path::new(path), branch, base_commit)
        {
            Ok(CleanupOutcome::DirtySkipped) => {
                // Turned dirty between decision and removal: the pane is
                // already gone, but data loss (irreversible) outranks a lost
                // pane (minor). The sweep retries the removal later.
                tracing::warn!(
                    task_id,
                    worktree = %path,
                    "worktree turned dirty after its pane was released; kept (F-23)"
                );
            }
            Ok(outcome) => {
                // Removed: this task is done being swept — drop its release
                // memos so `released_panes` stays bounded by the worktrees
                // still awaiting removal, not by every dispatch a long
                // `--watch` run ever made (same hygiene as
                // `drop_task_sessions`).
                self.forget_release_memos(task_id);
                tracing::info!(task_id, ?outcome, "worktree cleanup");
            }
            Err(e) => {
                tracing::warn!(task_id, "worktree cleanup failed: {e}");
            }
        }
        Ok(())
    }

    /// Drop every release memo belonging to `task_id` (#486).
    ///
    /// The memo is keyed by session row, so pruning by task means asking which
    /// rows the task has. A task that has been retried owns several, and
    /// leaving the older ones behind would grow the set for the life of the
    /// process. A lookup failure only skips the pruning — the memo is an
    /// optimisation, never correctness.
    pub(super) fn forget_release_memos(&mut self, task_id: i64) {
        match self.db.list_sessions(task_id) {
            Ok(sessions) => {
                for session in sessions {
                    self.released_panes.remove(&session.id);
                }
            }
            Err(e) => tracing::debug!(task_id, "could not prune release memos: {e}"),
        }
    }

    /// Release (close) a finished task's pane via `session/release` (#210).
    /// Best-effort: every failure only logs — a pane that could not be
    /// released must never block the worktree removal (an orphaned pane is
    /// `doctor`'s job, #211). Marks the task released once the RPC answered
    /// (whatever `released` says — `false` means "already gone or refused",
    /// both final) or when release is impossible for this run; a transport
    /// error leaves it unmarked so the next sweep retries.
    pub(super) async fn release_pane(
        &mut self,
        record: &TaskRecord,
        mode: ReleaseMode,
    ) -> PaneRelease {
        let session = match self.db.latest_session(record.id) {
            Ok(Some(session)) => session,
            Ok(None) => {
                // Never dispatched → no pane to release, and nothing to
                // remember: the memo is keyed by session row (#486).
                return PaneRelease::NotApplicable;
            }
            Err(e) => {
                tracing::warn!(
                    task_id = record.id,
                    "cannot resolve session for pane release: {e}"
                );
                return PaneRelease::Failed;
            }
        };
        // Once-ness is the caller's call, and only this function knows which
        // session row the memo is keyed by (#486). The two callers differ:
        // cleanup must not re-release on every sweep, while a re-dispatch needs
        // the pane *gone* and cannot take the memo as proof of that — a
        // recorded `released: false` also covers an identity refusal, which
        // leaves the pane alive.
        if mode == ReleaseMode::Once && self.released_panes.contains(&session.id) {
            return PaneRelease::NotApplicable;
        }
        let Some(agent) = self.plugins.agents.get(&session.plugin) else {
            // The owning plugin is not launched this run; that cannot change
            // until restart, so do not retry every sweep.
            tracing::debug!(
                task_id = record.id,
                plugin = %session.plugin,
                "pane release skipped: agent plugin not launched"
            );
            self.released_panes.insert(session.id);
            return PaneRelease::NotApplicable;
        };
        if !agent.capabilities().pane_control {
            // No pane to control (e.g. orca): nothing to release, ever.
            self.released_panes.insert(session.id);
            return PaneRelease::NotApplicable;
        }
        let params = SessionReleaseParams {
            session_id: session.session_id.clone(),
            // Identity guard against pane-id reuse: the worktree path is
            // unique per task and the DB is its source of truth. The label is
            // plugin-internal, so the orchestrator never composes one.
            expect_cwd: record.worktree_path.clone(),
            expect_label: None,
        };
        match agent
            .call::<_, SessionReleaseResult>(method::SESSION_RELEASE, &params)
            .await
        {
            Ok(result) => {
                self.released_panes.insert(session.id);
                if result.released {
                    tracing::info!(task_id = record.id, "pane released");
                    PaneRelease::Closed
                } else {
                    tracing::debug!(
                        task_id = record.id,
                        "pane not released (already gone or identity mismatch)"
                    );
                    PaneRelease::Untouched
                }
            }
            Err(e) => {
                tracing::warn!(task_id = record.id, "session/release failed: {e}");
                PaneRelease::Failed
            }
        }
    }
}

/// Whether a release may be skipped because this session was already settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReleaseMode {
    /// Cleanup (#210): a worktree removal that keeps failing must not re-send
    /// `session/release` on every sweep.
    Once,
    /// A precondition rather than cleanup (#481): the caller is about to open a
    /// new pane for this task and needs the old one gone. The memo is not
    /// evidence that it is — `released: false` covers "already gone" *and*
    /// "the identity guard refused", and only the second leaves a live pane.
    Always,
}

/// What a `session/release` attempt did, for callers that react to the outcome
/// rather than fire and forget (#481).
///
/// [`Untouched`](Self::Untouched) deliberately does **not** distinguish "the
/// pane was already gone" from "the identity guard refused to close it": the
/// RPC answers a bare `released: bool` for both. The plugin logs which one it
/// was — herdr warns `release refused: the pane id names a different pane now`
/// — and plugin logs are relayed, so the distinction is reachable without
/// widening the protocol for a case never yet observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneRelease {
    /// The plugin closed the pane.
    Closed,
    /// The plugin answered and closed nothing.
    Untouched,
    /// There was no pane to close: never dispatched, the owning plugin is not
    /// launched this run, or it does not control panes.
    NotApplicable,
    /// The attempt itself failed (session lookup or RPC).
    Failed,
}
