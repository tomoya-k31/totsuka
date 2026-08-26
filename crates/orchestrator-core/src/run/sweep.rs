//! Periodic maintenance the run loop performs between events (#464).
//!
//! Branch discovery for in-flight tasks, the read-only boundary check, and the
//! retention sweep for finished worktrees. All of it is throttled by
//! [`WORKTREE_SWEEP_INTERVAL`](super::WORKTREE_SWEEP_INTERVAL) rather than the
//! 200ms tick, because each one costs a git subprocess per task.

use super::*;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Read `HEAD` in the worktree of every in-flight task and record the
    /// branch the agent put it on.
    ///
    /// The periodic half of branch discovery. [`sync_branch`](Self::sync_branch)
    /// also runs on every Stop, which is the timely path — but a Stop is not
    /// guaranteed to arrive. A crash, a `SIGTERM`, an operator killing the
    /// pane, and `sweep_signal_timeouts`' own escalation all end a task without
    /// one, and codex and opencode send no intermediate heartbeat at all
    /// (`on-stop.sh` only emits one when `background_tasks` is non-empty, which
    /// opencode hardcodes empty). Leaving the branch unrecorded in those cases
    /// costs the branch cleanup and the re-creation path that reuses it.
    ///
    /// Rides the existing worktree-sweep throttle rather than the 200ms tick:
    /// this is one `git rev-parse` per running task, which is the same order of
    /// cost as the `git status` the sweep already throttles for.
    pub(super) async fn sync_branches_of_active_tasks(&mut self) -> Result<(), EngineError> {
        let mut candidates = Vec::new();
        // Every non-terminal state a dispatched task can sit in. `Escalated` is
        // the one this exists for — `sweep_signal_timeouts` escalates earlier
        // in the same `cycle()` and produces no Stop — but a task parked in
        // `WaitingInput` / `Verifying` / `Publishing` has a live worktree the
        // agent may still be branching in, and none of them is reached by a
        // Stop either once the pane is gone.
        for state in [
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::WaitingInput,
            TaskState::Verifying,
            TaskState::Escalated,
            TaskState::Publishing,
        ] {
            for record in self.db.tasks_in_state(state)? {
                candidates.push(record.id);
            }
        }
        for task_id in candidates {
            self.sync_branch(task_id)?;
            self.enforce_read_only(task_id).await?;
        }
        Ok(())
    }

    /// Fail an in-flight read-only task whose worktree is on a branch, and
    /// close its pane (#410, the last open item on it).
    ///
    /// [`finalize_success`](Self::finalize_success) already refuses to publish
    /// such a task, and that is the check that makes the violation impossible
    /// to *miss*. It is not enough on its own for two reasons, and this covers
    /// both:
    ///
    /// - **A task that never reaches publishing keeps the violation.** Ending
    ///   in `WaitingInput`, `Escalated`, or with the pane killed leaves a task
    ///   that branched, and possibly pushed, sitting in a non-failed state with
    ///   nothing but a log line. #422's live run is exactly that shape: an
    ///   `answer` task that stopped at `NEEDS_INPUT`.
    /// - **The agent is still running.** `finalize_success` sees the branch
    ///   after the agent has finished with it. Here the pane is closed, which
    ///   is the only lever this side has on an agent mid-run.
    ///
    /// **This is not prevention, and the interval says so.** The sweep runs at
    /// [`WORKTREE_SWEEP_INTERVAL`] (60s), so the window between `git switch -c`
    /// and `git push` — seconds — is not one this can be relied on to win.
    /// What it does guarantee is that a violating task ends up `failed` with
    /// its agent stopped, rather than continuing to work in a repository it was
    /// told not to touch.
    ///
    /// **Prevention is not coming.** #418 measured that a sandbox would work;
    /// building it was declined (#446, ADR-0045), so this detection is the
    /// strongest thing there is and read-only stays a best effort.
    ///
    /// The worktree and its commits are deliberately kept (`fail_publish`'s
    /// contract), so the evidence outlives the failure.
    async fn enforce_read_only(&mut self, task_id: i64) -> Result<(), EngineError> {
        let Some(record) = self.db.get_task(task_id)? else {
            return Ok(());
        };
        let profile = workflows_by_name(&self.settings.workflows)
            .get(record.workflow.as_str())
            .and_then(|w| w.profile);
        // The live `HEAD`, never `record.branch`: the column is write-once, so
        // gating on it would make `task retry` fail forever on a worktree the
        // operator had already detached (the trap #409 hit and documented in
        // `finalize_success`).
        let live_branch = record
            .worktree_path
            .as_deref()
            .filter(|p| Path::new(p).is_dir())
            .and_then(|p| self.worktrees.head_branch(Path::new(p)));
        let Some(reason) = read_only_side_effect(
            &record.workflow,
            profile,
            live_branch.as_deref(),
            record.id,
            record.worktree_path.as_deref().unwrap_or("<worktree>"),
        ) else {
            return Ok(());
        };
        // Try to stop the agent before recording the failure: while the pane
        // lives, every second is another chance for the push this check cannot
        // undo. **Best-effort, and the failure is recorded either way.** The
        // alternative — hold the task in-flight until the pane is confirmed
        // closed — trades the reliable half for the unreliable one: a herdr
        // that cannot be reached would leave the violation unrecorded, which is
        // the hole this whole check exists to close. An unreleasable pane is
        // `doctor`'s job by construction (#211), and the pane still carries the
        // ownership marker that `session/list` finds it by.
        // Read the outcome rather than the memo (#486): the memo is keyed by
        // session row now, and asking it here would mean resolving the row a
        // second time to learn what `release_pane` already returned.
        // `Refused` counts too (0.4.2, #485): it is the one answer that says a
        // pane is definitely still there, which is exactly what this warning
        // is about.
        if matches!(
            self.release_pane(&record, ReleaseMode::Once).await,
            PaneRelease::Failed | PaneRelease::Refused
        ) {
            tracing::error!(
                task_id = record.id,
                "could not confirm the pane closed: the agent may still be running. \
                 `totsuka doctor` lists it as an orphan pane"
            );
        }
        self.drop_task_sessions(record.id);
        self.fail_publish(&record, "read_only_violation", reason)
            .await
    }

    /// Record which branch a task's worktree is on, if it is on one.
    ///
    /// Creation hands the worktree over detached and the agent names the
    /// branch, so this read is how the orchestrator learns the name at all —
    /// there is no channel from the agent back to totsuka other than the status
    /// marker and the final message, and adding one would have to be
    /// implemented three times over (claude / codex / opencode) and trusted to
    /// report accurately. `HEAD` is the ground truth by construction.
    ///
    pub(super) fn sync_branch(&mut self, task_id: i64) -> Result<(), EngineError> {
        let Some(record) = self.db.get_task(task_id)? else {
            return Ok(());
        };
        let Some(path) = record
            .worktree_path
            .as_deref()
            .filter(|p| Path::new(p).is_dir())
        else {
            return Ok(());
        };
        // Detached is left unrecorded rather than cleared: the agent may simply
        // not have branched yet, and this runs repeatedly.
        let Some(head) = self.worktrees.head_branch(Path::new(path)) else {
            return Ok(());
        };
        if record.branch.as_deref() == Some(head.as_str()) {
            return Ok(());
        }
        tracing::info!(task_id, branch = %head, "recorded the agent's branch");
        if let Some(warning) = plan_mode_side_effect(&record.mode, &head) {
            tracing::warn!(task_id, branch = %head, "{warning}");
        }
        self.db.set_branch(task_id, &head)?;
        Ok(())
    }

    /// Re-apply the cleanup policy to finished tasks whose worktree still
    /// exists (F-23: a `retention_days` policy elapses long after the
    /// finishing run's immediate cleanup attempt retained the worktree).
    pub(super) async fn sweep_finished_worktrees(&mut self) -> Result<(), EngineError> {
        let mut candidates = Vec::new();
        // `Skipped` is included (#556): a failed task that a human retried
        // can lose the claim to another member and step aside *with the
        // worktree its failed run left behind* — nothing else sweeps it.
        for state in [TaskState::Done, TaskState::Cancelled, TaskState::Skipped] {
            for record in self.db.tasks_in_state(state)? {
                if record
                    .worktree_path
                    .as_deref()
                    .is_some_and(|p| Path::new(p).exists())
                {
                    candidates.push(record.id);
                }
            }
        }
        for task_id in candidates {
            self.cleanup_worktree(task_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worktree_sweep_is_throttled_by_its_interval() {
        // A long interval: the startup cycle (None) always sweeps, later
        // cycles within the interval do not.
        let mut engine = test_engine(Duration::from_secs(3600)).await;
        engine.cycle().await.unwrap();
        let first = engine
            .last_worktree_sweep
            .expect("the startup cycle sweeps");
        engine.cycle().await.unwrap();
        assert_eq!(
            engine.last_worktree_sweep,
            Some(first),
            "a cycle inside the interval must not re-sweep"
        );

        // Duration::ZERO restores the pre-#210 behavior: every cycle sweeps.
        let mut engine = test_engine(Duration::ZERO).await;
        engine.cycle().await.unwrap();
        let first = engine.last_worktree_sweep.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        engine.cycle().await.unwrap();
        assert!(
            engine.last_worktree_sweep.unwrap() > first,
            "a zero interval sweeps every cycle"
        );
    }
}
