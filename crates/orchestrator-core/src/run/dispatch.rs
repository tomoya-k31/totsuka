//! Getting a queued task in front of an agent (#464).
//!
//! Repository selection (F-10–F-14), slot-gated dispatch (F-40–F-43), and the
//! retry/re-attach path (F-44). The worktree is created here and handed over on
//! a detached `HEAD`; naming the branch is the agent's job (ADR-0026).

use super::*;
use crate::adapters::state_db::AUTO_RETRY_KIND;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Select a repository for every queued task that has none (F-10–F-14).
    pub(super) async fn select_repos(&mut self) -> Result<(), EngineError> {
        let queued = self.db.tasks_in_state(TaskState::Queued)?;
        for record in queued.iter().filter(|t| t.repo.is_none()) {
            let task = task_from_record(record);
            let decision = self.decide_repo(&task).await;
            match decision {
                RepoDecision::Selected { repo, reason } => {
                    tracing::info!(task_id = record.id, repo = %repo, "repository selected: {reason}");
                    self.db.set_repo(record.id, &repo)?;
                }
                RepoDecision::Pending { reason } => {
                    tracing::warn!(
                        task_id = record.id,
                        "repository pending confirmation: {reason}"
                    );
                    self.db.apply_event(
                        record.id,
                        TaskEvent::NeedRepoConfirmation,
                        Some(serde_json::json!({ "kind": "repo_select", "reason": reason })),
                    )?;
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::Pending,
                        record,
                        Some(reason),
                    );
                }
                RepoDecision::Failed { reason } => {
                    tracing::error!(task_id = record.id, "repository selection failed: {reason}");
                    self.db.apply_event(
                        record.id,
                        TaskEvent::Fail,
                        Some(serde_json::json!({ "kind": "repo_select", "reason": reason })),
                    )?;
                    self.stats.failed += 1;
                    self.write_back_status(record, false).await;
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::Failed,
                        record,
                        Some(reason),
                    );
                }
            }
        }
        Ok(())
    }

    /// Decide the repository for one task (rules first, LLM fallback).
    async fn decide_repo(&self, task: &Task) -> RepoDecision {
        let candidates: Vec<RepoCandidate> = self
            .settings
            .repos
            .iter()
            .map(|r| RepoCandidate {
                name: r.name.clone(),
                summary: r.summary.clone(),
                readme_head: self
                    .readme_cache
                    .as_ref()
                    .and_then(|c| c.head(&r.path, README_HEAD_LINES)),
            })
            .collect();
        match &self.llm {
            Some(llm) => select_repo(task, &candidates, llm, &self.settings.select).await,
            None => select_repo(task, &candidates, &NoLlmRouter, &self.settings.select).await,
        }
    }

    /// Log, and tell the operator once, that a task cannot start because a
    /// tool its profile needs is unusable here (#399).
    ///
    /// Reuses [`NotifierEvent::Pending`] rather than adding a variant. A new
    /// one would fail to deserialize in a notifier plugin built against the
    /// current protocol, and since delivery is fire-and-forget (F-93) the
    /// symptom would be **notifications quietly stopping** — worse than a
    /// slightly generic event name. A dedicated variant belongs with the
    /// `#[serde(other)]` fallback in protocol 0.3.
    async fn report_blocked_on_agent_tools(
        &mut self,
        record: &TaskRecord,
        missing: &[crate::agent_tools::AgentTool],
    ) {
        let names: Vec<&str> = missing.iter().map(|t| t.as_str()).collect();
        // Persisted so `totsuka status` can still answer "why is this task not
        // moving" long after the notification scrolled away (#407). Recorded
        // rather than recomputed at read time on purpose: `status` runs in the
        // operator's shell, where `gh` may well be on `PATH` even though it is
        // not here — a live check there would report "not blocked" about a
        // task this process is refusing to dispatch.
        //
        // **Ahead of the notification gate below, on every cycle.**
        // `note_task` deduplicates against the task's own history, which makes
        // repeating the call a single indexed lookup — and buys two things the
        // in-process set cannot give: a failed write is retried next cycle
        // rather than losing the explanation for the whole wait, and a
        // *changed* `missing` set supersedes the recorded one instead of
        // leaving a note that no longer describes the situation.
        let note = serde_json::json!({
            crate::adapters::state_db::NOTE_KEY: crate::agent_tools::BLOCKED_NOTE,
            "missing": names,
        });
        if let Err(e) = self.db.note_task(record.id, &note) {
            tracing::warn!(task_id = record.id, error = %e, "could not record the wait reason");
        }
        // The set gates the *notification* only (#399): interrupting someone
        // every 200 ms is spam, whereas re-recording the same note is a no-op.
        if !self.blocked_on_tools.insert(record.id) {
            tracing::debug!(
                task_id = record.id,
                missing = ?names,
                "still waiting on an unavailable agent tool"
            );
            return;
        }
        let reason = format!("waiting: {}", crate::agent_tools::blocked_reason(&names));
        tracing::warn!(task_id = record.id, missing = ?names, "{reason}");
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Pending,
            record,
            Some(reason),
        );
    }

    /// Dispatch queued tasks with a selected repository, gated by slots
    /// (F-40–F-43).
    pub(super) async fn dispatch_ready(&mut self) -> Result<(), EngineError> {
        // Cloned rather than borrowed: reporting a blocked task needs
        // `&mut self`, and holding a borrow of `self.settings` across the loop
        // would forbid it. Two small strings per workflow, once per cycle.
        let wf_info: HashMap<String, (String, Option<crate::config::Profile>)> = self
            .settings
            .workflows
            .iter()
            .map(|w| (w.name.clone(), (w.agent.clone(), w.profile)))
            .collect();
        let queued = self.db.tasks_in_state(TaskState::Queued)?;
        // Only a queued task can be parked, so the memo cannot outlive the
        // queue (#499). Pruning against it here — rather than removing at each
        // exit — is what makes the bound structural: a task cancelled at its
        // source, or one whose workflow was removed from config, never reaches
        // the loop below to clean up after itself, and a `--watch` process
        // runs for weeks.
        let still_queued: std::collections::HashSet<i64> = queued.iter().map(|r| r.id).collect();
        self.blocked_on_agent.retain(|id| still_queued.contains(id));
        let mut ready = Vec::new();
        for record in &queued {
            let Some(repo) = record.repo.clone() else {
                continue; // repo selection pending/failed this cycle
            };
            let Some((agent, profile)) = wf_info.get(record.workflow.as_str()).cloned() else {
                tracing::warn!(
                    task_id = record.id,
                    workflow = %record.workflow,
                    "workflow no longer configured; task stays queued → restore the workflow or cancel the task"
                );
                continue;
            };
            // #399: the agent writes its own deliverable now, so a task can
            // fail for a reason that is not about the task. Skipping here
            // rather than in `dispatch_one` means no slot is taken and no
            // worktree is created for work that cannot finish.
            //
            // **Skip, not fail.** The check runs in this process, while the
            // agent runs in a pane with the user's shell profile applied, so a
            // `gh` reachable only there reads as missing. Leaving the task
            // `Queued` makes a false negative a delay instead of a loss: it
            // dispatches on its own once the check passes.
            let missing = self.agent_tools.missing(profile, std::time::Instant::now());
            if !missing.is_empty() {
                self.report_blocked_on_agent_tools(record, &missing).await;
                continue;
            }
            // The wait ended, so the "already told you" memory has to end with
            // it: a task that is blocked, dispatched, retried and blocked
            // again is in a *new* condition the operator has not been told
            // about. Without this the set only ever grows and the second wait
            // is silent in both the notification and `status` (#407).
            self.blocked_on_tools.remove(&record.id);
            // #499: a plugin between instances must not take a slot either.
            // Same reason as the tool gate above — deciding here rather than
            // in `dispatch_one` means no slot is acquired, so tasks for
            // *other* agents keep flowing during an outage. `plan_dispatch`
            // below fixes the plan for the whole cycle, so a slot released
            // inside `dispatch_one` comes back too late to help anyone.
            //
            // The streak is only read when the agent is actually down, so the
            // healthy path costs no extra query.
            let status = self.agent_status_of(&agent);
            let down = status.as_ref().is_some_and(|s| !s.live && !s.abandoned);
            let spent = if down {
                self.spent_retries(record.id)
            } else {
                0
            };
            if agent_gate(&agent, status.as_ref(), spent) == AgentGate::Park {
                // Told once per task, not once per 200 ms tick — the same
                // reason `blocked_on_tools` exists. The operator's actionable
                // signal is the plugin's own escalation, which `supervise`
                // sends when the restart budget runs out.
                if self.blocked_on_agent.insert(record.id) {
                    tracing::warn!(
                        task_id = record.id,
                        agent = %agent,
                        "agent plugin is down; leaving the task queued until it is back"
                    );
                }
                continue;
            }
            // The wait ended (or never started), so the memo ends with it —
            // and the set stops growing. Doing it here rather than only in
            // `dispatch_one` also covers a task that never reaches dispatch
            // again: cancelled at its source, or its workflow removed.
            self.blocked_on_agent.remove(&record.id);
            ready.push(ReadyTask {
                task_id: record.id,
                repo,
                agent,
                priority: record.priority,
            });
        }
        let pair_by_id: HashMap<i64, (String, String)> = ready
            .iter()
            .map(|t| (t.task_id, (t.repo.clone(), t.agent.clone())))
            .collect();
        for task_id in plan_dispatch(&mut self.slots, &ready) {
            if let Some(pair) = pair_by_id.get(&task_id) {
                self.slot_holders.insert(task_id, pair.clone());
            }
            self.dispatch_one(task_id).await?;
        }
        Ok(())
    }

    /// Get the worktree this dispatch will hand over: reuse the recorded one
    /// when it is still on disk, else create it (#471, split out of
    /// `dispatch_one`).
    ///
    /// `Ok(None)` means the task was already failed through
    /// `fail_dispatch` — the reasons here are the ones an operator can act
    /// on, so they keep going through the same path as before.
    async fn acquire_worktree(
        &mut self,
        record: &TaskRecord,
        repo: &RepoSettings,
    ) -> Result<Option<PathBuf>, EngineError> {
        // Worktree: reuse a recorded one (retry without a live session), else
        // create fresh (F-20–F-22).
        //
        // The recorded path must still be **on disk** (#254). Cleanup removes
        // it at completion under `plan_cleanup = "immediate"`, and an operator
        // may remove it by hand, so a recorded path is not evidence of a usable
        // worktree; handing a missing directory to the agent fails the dispatch
        // for a reason the operator cannot act on. Re-creating renders the same
        // path (a pure function of source + task id) and puts the worktree back
        // on the branch this task was last seen on, and the agent session
        // survives it: Claude Code keys sessions by working directory, storing
        // them outside the worktree.
        //
        // Keyed on the path alone: a recorded worktree is reusable whether or
        // not a branch was ever recorded for it, and requiring both would send
        // a branchless task back through `create` to collide with its own
        // directory.
        let path = match &record.worktree_path {
            Some(path) if Path::new(path).is_dir() => PathBuf::from(path),
            _ => {
                let location_template = repo
                    .worktree_location
                    .clone()
                    .unwrap_or_else(|| self.settings.location_template.clone());
                let request = CreateRequest {
                    repo_path: &repo.path,
                    repo_name: &repo.name,
                    source: &record.source,
                    task_id: &record.source_task_id,
                    existing_branch: record.branch.as_deref(),
                    name_template: &self.settings.worktree_name_template,
                    location_template: &location_template,
                    base_branch: None,
                    env: &self.settings.env,
                };
                match self.worktrees.create(&request) {
                    Ok(worktree) => {
                        let path = worktree.path.display().to_string();
                        self.db.set_worktree(
                            record.id,
                            &path,
                            worktree.branch.as_deref(),
                            &worktree.base_commit,
                        )?;
                        worktree.path
                    }
                    // `AlreadyExists` means the rendered path is claimed by a
                    // worktree this task does not own — a leftover from an
                    // interrupted run, or an operator's own checkout. Say so
                    // and name the remedy instead of surfacing raw git stderr:
                    // re-creation (#254) already absorbs every case totsuka
                    // caused itself, so reaching here needs a human.
                    Err(WorktreeError::AlreadyExists { path }) => {
                        return self
                            .fail_dispatch(
                                record,
                                format!(
                                    "`{}` is already occupied but is not recorded for this task; \
                                     remove it — `git worktree remove {}`, or the cleanup \
                                     `totsuka doctor` offers, or plain `rm -rf` if it is not a \
                                     worktree at all — and retry",
                                    path.display(),
                                    path.display(),
                                ),
                            )
                            .await
                            .map(|()| None);
                    }
                    Err(e) => {
                        return self
                            .fail_dispatch(record, e.to_string())
                            .await
                            .map(|()| None);
                    }
                }
            }
        };
        Ok(Some(path))
    }

    /// Wire a hook-capable agent for this dispatch: reserve the session row
    /// the `job_id` names, assemble the launch env, and decide whether the
    /// completion contract travels invisibly or as visible context (#471,
    /// split out of `dispatch_one`).
    ///
    /// Returns all-`None` for an agent without hooks (orca / mock), which is
    /// the unchanged path.
    #[allow(clippy::type_complexity)]
    async fn wire_hooks(
        &mut self,
        record: &TaskRecord,
        agent_name: &str,
        tool_profile: &crate::tool::ToolProfile,
        task: &Task,
        on_a_branch: bool,
        resume_session_id: Option<&str>,
    ) -> Result<
        (
            Option<String>,
            Option<(String, std::collections::BTreeMap<String, String>)>,
            Option<i64>,
            Option<String>,
        ),
        EngineError,
    > {
        // Agents declaring `hook_completion` (0.5.0, #496 — herdr does)
        // receive a correlation `job_id` + a [`HookLaunchSpec`] so their Claude
        // Code hooks POST completion signals back (#131/#138). The job id's
        // `session_row` must exist *before* launch — it is injected into the
        // process and echoed by every hook — so the session row is reserved up
        // front and its native id filled in after `task/dispatch` returns.
        // Non-hook agents (orca / mock) take the unchanged path below.
        let hook_capable = self
            .plugins
            .agents
            .get(agent_name)
            .map(|a| a.capabilities().hook_completion)
            .unwrap_or(false);
        let wiring = match hook_capable
            .then(|| self.hook_launch(&record.workflow))
            .flatten()
        {
            Some((settings_path, mut env)) => {
                let session_row = self.db.reserve_session(record.id, agent_name)?;
                // Thread continuity (#140's D-10, superseded by #242 — the
                // session to resume is now this task's own): tentatively stamp the resumed
                // Claude session id onto the fresh row so a later follow-up can
                // resume it even before this dispatch's SessionStart hook lands
                // (best-effort resilience). The hook's SessionStart reconciles
                // it against the real id (#138: a `--resume` may legitimately
                // change the id → warn + keep the newest).
                if let Some(sid) = resume_session_id {
                    self.db.set_tool_session_id(session_row, sid)?;
                }
                let job_id = JobId::new(record.id, session_row);
                env.insert("TOTSUKA_JOB_ID".to_string(), job_id.to_string());
                // Invisible prompt context: the task-source's `instructions`
                // (0.1.5) plus the marker self-report convention ride the
                // `UserPromptSubmit` hook's `additionalContext` via this env
                // var — the model sees them, the pane shows only the task
                // body. Hook knowledge stays in core (H-01): source plugins
                // never compose marker instructions.
                let prompts = self.settings.prompts.for_workflow(&record.workflow);
                let mut prompt_context = String::new();
                if let Some(instructions) = &task.instructions {
                    prompt_context.push_str(instructions);
                    prompt_context.push_str("\n\n");
                }
                // Ask the agent to branch, but only where the ask is both
                // actionable and needed. Plan mode is *meant* to be read-only
                // (claude `--permission-mode plan`, codex `--sandbox
                // read-only`, opencode's `bash: deny`) and on claude the
                // instruction would provoke a permission prompt an unattended
                // pane has nobody to answer — turning an unfollowable ask into
                // a timeout escalation. **That read-only-ness is not
                // enforced**: a live plan task branched, committed, pushed and
                // opened a PR because its repository's conventions asked for
                // it (#378), which `plan_mode_side_effect` now reports. Not
                // injecting the ask here is therefore about not *provoking*
                // git, not about git being impossible. A task already on a
                // branch is resuming onto one this task made earlier, and
                // re-asking would invite a second branch mid-conversation.
                if record.mode != "plan" && !on_a_branch {
                    prompt_context.push_str(prompts.branch_convention());
                    prompt_context.push_str("\n\n");
                }
                // The confirm-with-a-human self-report has a question-tool
                // variant (#487): on a tool with a native single-select
                // question tool (claude `AskUserQuestion`, opencode
                // `question`) the agent asks through that UI instead of
                // parking the turn with NEEDS_INPUT. Selected here, not in
                // `resolve_for` — tool resolution has a repository dimension,
                // so the same workflow can dispatch to different tools.
                match tool_profile
                    .capabilities()
                    .interactive_question
                    .and_then(|qt| prompts.marker_self_report_for_question_tool(qt))
                {
                    Some(text) => prompt_context.push_str(&text),
                    None => prompt_context.push_str(prompts.marker_self_report()),
                }
                // Context routing per tool capability (#196 Phase 3): a tool
                // without invisible injection (opencode — no UserPromptSubmit
                // additionalContext channel) gets the same instructions +
                // marker convention as *visible* extra_context instead, so
                // the completion contract still reaches the model up front.
                let visible_hook_context = if tool_profile.capabilities().invisible_injection {
                    env.insert("TOTSUKA_PROMPT_CONTEXT".to_string(), prompt_context);
                    None
                } else {
                    Some(prompt_context)
                };
                (
                    Some(job_id.to_string()),
                    Some((settings_path, env)),
                    Some(session_row),
                    visible_hook_context,
                )
            }
            None => (None, None, None, None),
        };
        Ok(wiring)
    }

    /// Record a dispatch that succeeded and start listening (#471, split out
    /// of `dispatch_one`).
    ///
    /// Everything here happens **after** the agent accepted the task, in the
    /// order that keeps a crash recoverable: the native session id lands
    /// first (F-37, so a restart can re-attach), the audit event next, the
    /// message ledger only once the batch is genuinely in the agent's hands
    /// (#242 — stamping earlier would lose the batch on a failed dispatch),
    /// and `state/subscribe` last (F-38).
    async fn record_dispatch_and_subscribe(
        &mut self,
        record: &TaskRecord,
        agent_name: &str,
        dispatched: &plugin_protocol::methods::TaskDispatchResult,
        reserved_row: Option<i64>,
        pending: &[TaskMessage],
        worktree_path: &Path,
    ) -> Result<(), EngineError> {
        let agent = self
            .plugins
            .agents
            .get(agent_name)
            .expect("the agent was resolved before dispatch");
        // Persist the native session id (F-37): fill the reserved hook row, or
        // append a fresh row on the non-hook path.
        match reserved_row {
            Some(row) => self.db.set_session_native_id(row, &dispatched.session_id)?,
            None => {
                self.db
                    .record_session(record.id, agent_name, &dispatched.session_id)?;
            }
        }
        self.db.apply_event(
            record.id,
            TaskEvent::Dispatch,
            Some(serde_json::json!({
                "kind": "dispatch", "plugin": agent_name, "session_id": dispatched.session_id,
            })),
        )?;
        // The messages are in the agent's hands now. Stamped only after the
        // dispatch succeeded, so a failed one leaves them queued for the
        // retry; every message in this batch gets the same timestamp, which is
        // what lets `task retry` later put exactly this batch back (#257).
        if !pending.is_empty() {
            self.db.mark_messages_processed(record.id)?;
        }
        self.sessions.insert(
            (agent_name.to_string(), dispatched.session_id.clone()),
            record.id,
        );
        self.stats.dispatched += 1;
        tracing::info!(
            task_id = record.id,
            agent = %agent_name,
            session_id = %dispatched.session_id,
            worktree = %worktree_path.display(),
            "dispatched"
        );

        let subscribe = plugin_protocol::methods::StateSubscribeParams {
            session_id: dispatched.session_id.clone(),
        };
        let subscribe_error = match agent
            .call::<_, Value>(method::STATE_SUBSCRIBE, &subscribe)
            .await
        {
            Ok(_) => None,
            Err(e) => {
                // No stream means the task could never progress (the loop
                // would hold its slot forever). Best-effort cancel, then fail.
                let cancel = plugin_protocol::methods::TaskCancelParams {
                    session_id: dispatched.session_id.clone(),
                };
                let _ = agent.call::<_, Value>(method::TASK_CANCEL, &cancel).await;
                Some(e.to_string())
            }
        };
        if let Some(e) = subscribe_error {
            self.drop_task_sessions(record.id);
            return self
                .fail_dispatch(
                    record,
                    format!("state/subscribe failed: {e} → dispatch cancelled; fix the agent plugin and `task retry`"),
                )
                .await;
        }
        Ok(())
    }

    /// Dispatch a single task: worktree (create or reuse), `task/dispatch` (or
    /// `session/attach` on retry reuse, F-44), `state/subscribe`. The slot has
    /// already been acquired; failure paths release it and fail the task.
    ///
    /// The decision is [`resolve_dispatch_target`]; what remains here is the
    /// order the side effects have to happen in (#471).
    /// How many automatic dispatch retries a task has spent (#492).
    ///
    /// A read failure counts as "none spent", biasing toward waiting for a
    /// plugin that is coming back — the cheaper mistake, since the task stays
    /// queued rather than dying. **But it is logged**: without that, a DB
    /// fault would express itself as tasks quietly not moving, with nothing
    /// anywhere saying why. That is the failure shape this whole area exists
    /// to remove.
    fn spent_retries(&self, task_id: i64) -> u32 {
        match self.db.auto_retry_streak(task_id) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    task_id,
                    "could not read the retry streak: {e} → treating it as 0, so this task \
                     may wait for its agent instead of exhausting its budget"
                );
                0
            }
        }
    }

    /// The agent gate's view of one plugin (#495/#499).
    fn agent_status_of(&self, agent: &str) -> Option<AgentStatus> {
        self.plugins.agents.get(agent).map(|a| AgentStatus {
            capabilities: a.capabilities().clone(),
            live: !a.is_closed(),
            abandoned: self.abandoned_plugins.contains(agent),
        })
    }

    async fn dispatch_one(&mut self, task_id: i64) -> Result<(), EngineError> {
        let record = self
            .db
            .get_task(task_id)?
            .ok_or(StateError::NotFound(task_id))?;

        // Everything decidable before the first side effect, decided in one
        // pure function (#471). Failure handling stays here because it writes.
        // How many automatic dispatch retries this task has already spent
        // (#492). Read here rather than inside the pure resolver, which does
        // not touch the DB.
        let spent_retries = self.spent_retries(task_id);
        let target = resolve_dispatch_target(
            &record,
            &self.settings,
            |name| self.agent_status_of(name),
            spent_retries,
        );
        // Parking is the only outcome that leaves the task waiting on a downed
        // agent. Every other one — dispatching, failing, a vanished workflow —
        // ends that wait, so the "already reported" memo is dropped once, here,
        // instead of in each arm below. Structuring it this way is what keeps
        // the set from growing for the life of a `--watch` process, and keeps
        // a *later* outage reportable again.
        let target = match target {
            Err(DispatchRefusal::AgentDown) => {
                self.release_slot(task_id);
                // Told once per task, not once per 200 ms tick — the same
                // reason `blocked_on_tools` exists. The operator's actionable
                // signal is the plugin's own escalation, which `supervise`
                // sends when the restart budget runs out.
                if self.blocked_on_agent.insert(task_id) {
                    // The workflow's agent, not the workflow: a field named
                    // `agent` holding a workflow name is worse than no field.
                    let agent = workflows_by_name(&self.settings.workflows)
                        .get(record.workflow.as_str())
                        .map(|w| w.agent.clone())
                        .unwrap_or_default();
                    tracing::warn!(
                        task_id,
                        agent = %agent,
                        "agent plugin is down; leaving the task queued until it is back"
                    );
                }
                return Ok(());
            }
            other => {
                self.blocked_on_agent.remove(&task_id);
                other
            }
        };
        let DispatchTarget {
            agent_name,
            profile: wf_profile,
            initial_prompt,
            repo,
            tool_name: _tool_name,
            tool_profile,
        } = match target {
            Ok(target) => target,
            Err(DispatchRefusal::UnknownWorkflow) => {
                self.release_slot(task_id);
                return Ok(()); // warned in dispatch_ready
            }
            Err(DispatchRefusal::Failed(reason)) => {
                return self.fail_dispatch(&record, reason).await;
            }
            // Consumed by the park arm above; unreachable here.
            Err(DispatchRefusal::AgentDown) => unreachable!("parked above"),
        };

        // Conversation continuity (#242, superseding #140's D-10): a follow-up
        // message reopens *this* task, so the session to resume is simply this
        // task's own most recent one — no cross-task search. Read before
        // `reserve_session` below, which would otherwise make the empty row it
        // creates the "latest".
        //
        // Best-effort: a missing or empty id yields `None` and dispatches
        // fresh, with no warning. Gated on the tool's capabilities (#196): a
        // tool that cannot resume, or whose native session id is never
        // captured, always dispatches fresh.
        //
        // E-09: the reply destination is always the task's own
        // `source_task_id` (task_id-origin routing via `job_id`); nothing here
        // — or anywhere — derives a destination from a tool session id.
        let latest = self.db.latest_session(record.id)?;
        let tool_caps = tool_profile.capabilities();
        let resume_session_id = if tool_caps.resume && tool_caps.session_id_capture {
            latest
                .as_ref()
                .and_then(|s| s.tool_session_id.clone())
                .filter(|sid| !sid.is_empty())
        } else {
            None
        };

        // Message-driven prompt (#242): what the agent is asked to do is the
        // messages nobody has sent it yet, concatenated oldest-first, so a
        // burst of three replies produces one dispatch and one answer rather
        // than three panes. A resumed session already holds everything before
        // them, so only the new ones go.
        //
        // An empty ledger falls back to the record's own body — the shape
        // pre-#242 tasks were stored in, and what v6's backfilled rows leave
        // behind.
        let pending = self.db.pending_task_messages(record.id)?;

        // Retry reuse (F-44): a surviving worktree + session resumes the
        // existing conversation instead of dispatching anew.
        //
        // Only when there is nothing new to say. Re-attaching hands the agent
        // no prompt, and since #242 a reopened conversation *always* looks
        // reusable (`retry_plan` reads worktree + branch + session, never the
        // task's state), so without this guard a follow-up message would be
        // swallowed: no dispatch, nothing marked processed, and the agent
        // never told. Before #242 a follow-up was a different row with no
        // worktree of its own, which is why this never bit.
        if pending.is_empty()
            && let RetryPlan::ReuseSession {
                plugin, session_id, ..
            } = recovery::retry_plan(&record, latest.as_ref())
            && plugin == agent_name
            && let Some(state) = self.try_reattach(&plugin, &session_id).await
        {
            self.db.apply_event(
                record.id,
                TaskEvent::Dispatch,
                Some(serde_json::json!({
                    "kind": "dispatch", "reused_session": session_id, "plugin": plugin,
                })),
            )?;
            self.sessions
                .insert((plugin.clone(), session_id), record.id);
            self.stats.dispatched += 1;
            self.apply_agent_state(record.id, &plugin, state, None)
                .await?;
            return Ok(());
        }

        // A previous dispatch's pane may still be alive here (#481). `totsuka
        // task cancel` only writes the DB — the CLI has no plugin host, so it
        // cannot close anything — and the sweep that would release the pane
        // runs on `worktree_sweep_interval`. Retrying inside that window
        // dispatches on top of a live pane, and an agent plugin that derives
        // its agent name from the task id then collides with itself: herdr
        // answers `agent_name_taken`, the dispatch fails in under a second,
        // and no later sweep un-fails it. The same collision is reachable
        // without a retry — a follow-up message (#242) on a `done` task whose
        // pane outlived its worktree removal.
        //
        // So the release is owed by the dispatcher, not by a sweep that may or
        // may not have run. `release` rather than `task/cancel`: it carries the
        // `expect_cwd` identity guard, and herdr pane ids are position-based —
        // an unguarded close can take down a pane the id no longer names. It is
        // idempotent (an absent pane answers `released: false`), so the common
        // case where the sweep got there first costs one RPC.
        //
        // Only for a task that has been dispatched before, and only *before*
        // `wire_hooks` reserves a fresh session row: after that,
        // `latest_session` is the new empty row and the live pane is
        // unreachable.
        if latest.is_some() {
            // Logged here rather than left to `release_pane`'s own lines: this
            // is the one caller whose outcome has a *user-visible* consequence
            // (the dispatch below may be refused). `Untouched` stays quiet — it
            // is the ordinary case where the sweep already closed the pane.
            match self.release_pane(&record, ReleaseMode::Always).await {
                PaneRelease::Closed => tracing::info!(
                    task_id = record.id,
                    "closed the previous dispatch's pane before re-dispatching"
                ),
                // The plugin looked and found a pane of its own still sitting
                // on this task's worktree (protocol 0.4.2, #485) — not at the
                // id we recorded, which is why it would not close it, but
                // there. Dispatching anyway is how #481 looked from the
                // outside: an agent plugin that derives its agent name from
                // the task id refuses the launch with an error of its own
                // making, seconds later, in its own vocabulary. Stopping here
                // costs the same dispatch and buys a reason the operator can
                // act on.
                //
                // Only reachable against a plugin new enough to say why:
                // before 0.4.2, and for any reason this build does not know,
                // the answer is `Untouched` and this arm never runs.
                PaneRelease::Refused => {
                    return self
                        .fail_dispatch(
                            &record,
                            concat!(
                                "a pane is still open on this task's worktree — the ",
                                "agent plugin found one of its own there, at a ",
                                "different pane id than the one recorded, so it ",
                                "declined to close it → close that pane yourself, ",
                                "then `totsuka task retry` this task. `totsuka ",
                                "doctor` will not list it: its worktree still ",
                                "exists, so it is not an orphan"
                            )
                            .to_string(),
                        )
                        .await;
                }
                PaneRelease::Failed => tracing::warn!(
                    task_id = record.id,
                    "could not confirm the previous dispatch's pane is closed; if the agent \
                     plugin now refuses this dispatch because the session already exists, \
                     that pane is why"
                ),
                PaneRelease::Untouched | PaneRelease::NotApplicable => {}
            }
        }

        let Some(worktree_path) = self.acquire_worktree(&record, &repo).await? else {
            return Ok(());
        };

        // Whether the worktree we are about to hand over is on a branch,
        // read from the worktree itself rather than from `record`.
        //
        // `record` was fetched once at the top of this function and is stale by
        // now in exactly the case that matters: a re-creation whose recorded
        // branch survived neither locally nor on `origin` gets a *detached*
        // worktree and a `branch = NULL` write, while the in-memory copy still
        // says `Some(...)`. Deciding from that copy would suppress the branch
        // instruction for a worktree that is detached — the one failure this is
        // here to prevent. `HEAD` cannot be stale.
        let on_a_branch = self.worktrees.head_branch(&worktree_path).is_some();

        let mut task = task_from_record(&record);
        if let Some(body) = conversation_prompt(&pending) {
            task.body = Some(body);
        }
        // Where the deliverable goes, for a workflow whose deliverable is a
        // *new tracker item* (#542). Appended to the task-source's own
        // instructions rather than delivered on a channel of its own, so it
        // travels wherever those already travel — invisibly through the hook's
        // prompt context, or visibly for a tool with no invisible channel.
        //
        // `triage` only: it is the one profile whose output is an item filed
        // somewhere else. An `implement` run works in the repository it was
        // given, and telling it about a board would be noise it might act on.
        if wf_profile == Some(Profile::Triage)
            && let Some(destination) = self.claim_registry().destination(&repo.name)
        {
            let block = self
                .settings
                .prompts
                .for_workflow(&record.workflow)
                .tracker_destination(destination);
            task.instructions = Some(match task.instructions.take() {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }
        let (job_id, hook_spec, reserved_row, visible_hook_context) = self
            .wire_hooks(
                &record,
                &agent_name,
                &tool_profile,
                &task,
                on_a_branch,
                resume_session_id.as_deref(),
            )
            .await?;

        // task/dispatch (F-31) → session id → persist (F-37) → subscribe (F-38).
        let agent = self.plugins.agents.get(&agent_name).expect("checked above");
        // Context routing: hook dispatches deliver everything invisibly via
        // `TOTSUKA_PROMPT_CONTEXT` above when the tool supports it; a tool
        // without invisible injection got the same content as
        // `visible_hook_context` instead. Non-hook dispatches (orca / mock)
        // have no invisible channel — fall back to the task's instructions as
        // visible string extra_context (no marker convention: non-hook agents
        // don't report completion through hooks).
        let extra_context = match (&hook_spec, visible_hook_context) {
            // Hook dispatch, tool without invisible injection: the context is
            // delivered visibly (see above).
            (Some(_), Some(ctx)) => Some(serde_json::Value::String(ctx)),
            (Some(_), None) => None,
            (None, _) => task.instructions.clone().map(serde_json::Value::String),
        };
        let mode = execution_mode(&record.mode);
        // Fully-resolved tool launch (#196): the argv (base command, mode
        // flags, hook settings, resume id) is assembled in core from the
        // resolved profile; the plugin launches it verbatim. This is the only
        // launch channel since protocol 0.4.0 (#411) — the `hook` spec that
        // used to ride along for pre-0.2.3 plugins is gone, and `hook_spec`
        // below is a core-internal `(settings_path, env)` pair, not a wire
        // type. `tool_launch` bakes the resume flag into the argv, so a retry
        // without resume has to rebuild the whole spec — hence a closure
        // rather than a mutated struct.
        let build_params = |resume: Option<String>| TaskDispatchParams {
            task: task.clone(),
            worktree_path: worktree_path.display().to_string(),
            mode,
            // `initial_prompt` (#415) rides the *visible* channel, ahead of
            // everything else, and only when the agent is about to start a
            // fresh conversation. The test is `resume.is_none()` and it is
            // deliberately inside this closure: the `SESSION_UNRESUMABLE`
            // retry below rebuilds the params without a resume id, and that
            // dispatch really is a new conversation — the agent remembers
            // nothing, so the instructions have to come back with it.
            extra_context: prepend_initial_prompt(
                extra_context.clone(),
                initial_prompt.as_deref().filter(|_| resume.is_none()),
            ),
            job_id: job_id.clone(),
            tool_launch: tool_profile.launch_spec(&LaunchInputs {
                plan: mode == plugin_protocol::methods::ExecutionMode::Plan,
                profile: wf_profile,
                settings_path: hook_spec.as_ref().map(|(path, _)| path.as_str()),
                resume_session_id: resume.as_deref(),
                env: hook_spec
                    .as_ref()
                    .map(|(_, env)| env.clone())
                    .unwrap_or_default(),
            }),
            resume_session_id: resume,
            // 0.4.1 (#417): for the IDE plugin to show which repository the
            // agent is in. The *configured* name, not the worktree's directory
            // name, so the sidebar says what the logs and `totsuka status`
            // already say.
            repo_name: Some(repo.name.clone()),
        };
        let params = build_params(resume_session_id.clone());
        let mut attempt = agent.call(method::TASK_DISPATCH, &params).await;
        // `SESSION_UNRESUMABLE` (0.2.4, #242): the session we asked to resume
        // is gone. Resuming is an optimization — the work itself does not
        // depend on it — so drop it and dispatch once more. The retry cannot
        // fail the same way (it names no session), so this never loops.
        if let (Err(HostError::Rpc { code, message, .. }), Some(sid)) =
            (&attempt, &resume_session_id)
            && *code == plugin_protocol::error_code::SESSION_UNRESUMABLE
        {
            tracing::warn!(
                task_id = record.id,
                tool_session_id = %sid,
                "session could not be resumed ({message}); dispatching fresh — \
                 the agent starts without the earlier conversation"
            );
            attempt = agent.call(method::TASK_DISPATCH, &build_params(None)).await;
        }
        let dispatched: TaskDispatchResult = match attempt {
            Ok(result) => result,
            Err(e) => {
                // Roll back the pre-dispatch session reservation (hook path) so
                // a failed dispatch never leaves an empty-id row for retry /
                // recovery to re-attach to.
                if let Some(row) = reserved_row
                    && let Err(err) = self.db.delete_session(row)
                {
                    tracing::warn!(
                        task_id = record.id,
                        "failed to roll back reserved session row: {err}"
                    );
                }
                return self.fail_dispatch(&record, e.to_string()).await;
            }
        };
        self.record_dispatch_and_subscribe(
            &record,
            &agent_name,
            &dispatched,
            reserved_row,
            &pending,
            &worktree_path,
        )
        .await?;
        Ok(())
    }

    /// Requeue conversations that **completed** while messages were still
    /// unsent (#242).
    ///
    /// Ingest deliberately leaves a message alone when the conversation is
    /// mid-flight — interrupting a working agent to hand it one more line
    /// would mean a second pane and a second answer. Something has to notice
    /// once that dispatch is over, and this is that something.
    ///
    /// Only `Done`. The two other terminal states are a human's business:
    ///
    /// - `Failed` would be a **loop**. A dispatch that fails leaves its
    ///   messages unsent (they are stamped only on success), so requeueing on
    ///   failure would re-dispatch, fail the same way, and go round again
    ///   every tick — with a notification each time — for exactly the errors
    ///   that need a person (no repository, agent plugin down, unknown tool).
    ///   `totsuka task retry` is the deliberate way back, and it brings the
    ///   messages with it.
    /// - `Cancelled` would override the human. A message arriving *after* a
    ///   cancel still reopens the conversation through ingest — that is a
    ///   fresh instruction — but one that was already sitting in the ledger
    ///   when they cancelled must not undo them.
    pub(super) async fn requeue_conversations_with_unsent_messages(
        &mut self,
    ) -> Result<(), EngineError> {
        for task_id in self
            .db
            .conversations_with_unsent_messages(TaskState::Done)?
        {
            self.db.apply_event(
                task_id,
                TaskEvent::Reopen,
                Some(serde_json::json!({
                    "kind": "reopen", "cause": "messages_arrived_while_working",
                })),
            )?;
            tracing::info!(
                task_id,
                "conversation requeued: messages arrived while it was working"
            );
        }
        Ok(())
    }

    /// Try to re-attach to a session for retry reuse; `None` means dispatch
    /// fresh instead (lost session / attach failure).
    async fn try_reattach(&self, plugin: &str, session_id: &str) -> Option<AgentState> {
        use crate::ports::agent_session::AgentSession;
        let attacher = crate::adapters::PluginAgentSession::new(&self.plugins.agents);
        match attacher.attach(plugin, session_id).await {
            Ok(AttachOutcome::Attached(state)) => Some(state),
            Ok(AttachOutcome::Lost) => None,
            Err(e) => {
                tracing::warn!(plugin, session_id, "retry re-attach failed: {e}");
                None
            }
        }
    }

    /// Release the slot a task holds, if it holds one (per-task ledger).
    pub(super) fn release_slot(&mut self, task_id: i64) {
        if let Some((repo, agent)) = self.slot_holders.remove(&task_id) {
            self.slots.release(&repo, &agent);
        }
    }

    /// Drop a finished task's session routes so long-running `--watch` does
    /// not accumulate stale `(plugin, session_id)` entries.
    pub(super) fn drop_task_sessions(&mut self, task_id: i64) {
        self.sessions.retain(|_, &mut id| id != task_id);
    }

    /// Fail a task during dispatch: release its slot, record the reason,
    /// notify (F-90).
    async fn fail_dispatch(
        &mut self,
        record: &TaskRecord,
        reason: String,
    ) -> Result<(), EngineError> {
        tracing::error!(task_id = record.id, "dispatch failed: {reason}");
        self.release_slot(record.id);
        self.agent_output.remove(&record.id);
        // Recorded before deciding whether to retry, so the reason for *every*
        // attempt is in the log even when the task recovers and nobody ever
        // looks. It is also what makes the requeue below legal: `Retry` is a
        // transition out of `Failed`.
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": "dispatch", "reason": reason })),
        )?;

        // #492: most dispatch failures are transient — an agent CLI whose
        // launch keystrokes were swallowed, a plugin that timed out — and the
        // human's whole repair is `totsuka task retry`, unchanged, which then
        // works. Do that automatically, a bounded number of times.
        //
        // **The bound is the whole design.** `requeue_conversations_with_unsent_messages`
        // refuses to touch `Failed` because requeueing on failure with no limit
        // re-dispatches, fails the same way and goes round every tick forever
        // (see its rustdoc). Counting removes exactly that: the worst a
        // permanently broken task costs is `DISPATCH_RETRY_LIMIT` attempts.
        // Which is also why nothing here inspects *what* failed — with a bound
        // in place, misclassifying a failure as permanent (and abandoning a
        // task that one more attempt would have fixed) is the more expensive
        // mistake.
        match self.db.auto_retry_streak(record.id) {
            Ok(spent) if spent < DISPATCH_RETRY_LIMIT => {
                let attempt = spent + 1;
                let detail = serde_json::json!({
                    "kind": AUTO_RETRY_KIND,
                    "attempt": attempt,
                    "limit": DISPATCH_RETRY_LIMIT,
                });
                match self.db.retry_task(record.id, Some(detail)) {
                    Ok(_) => {
                        // No notification, no status write-back: the task is
                        // not finished, and telling the operator about a
                        // failure that repairs itself is the noise this
                        // feature exists to remove.
                        tracing::warn!(
                            task_id = record.id,
                            attempt,
                            limit = DISPATCH_RETRY_LIMIT,
                            "dispatch failed; requeued automatically"
                        );
                        return Ok(());
                    }
                    // Requeueing is the optimisation, not the contract. If the
                    // DB refuses, fall through and fail the task as before
                    // rather than leaving it in `Failed` with nobody told.
                    Err(e) => tracing::warn!(
                        task_id = record.id,
                        "could not requeue a failed dispatch: {e}"
                    ),
                }
            }
            Ok(_) => tracing::warn!(
                task_id = record.id,
                limit = DISPATCH_RETRY_LIMIT,
                "dispatch failed and the automatic retries are spent"
            ),
            Err(e) => tracing::warn!(
                task_id = record.id,
                "could not count previous automatic retries: {e}"
            ),
        }

        self.stats.failed += 1;
        self.write_back_status(record, false).await;
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Failed,
            record,
            Some(reason),
        );
        Ok(())
    }
}

/// How many times a dispatch failure is retried before the task is failed for
/// good (#492).
///
/// A constant, not a setting: the number only has to be large enough to ride
/// out a transient launch failure and small enough that a permanently broken
/// task stops quickly, and no observation yet says three is the wrong answer.
/// [`hooks.block_retry_limit`](crate::run::EngineSettings) is the precedent for
/// promoting it to config if one ever does.
const DISPATCH_RETRY_LIMIT: u32 = 3;

/// Why a dispatch is refused before anything is created (#471).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DispatchRefusal {
    /// The workflow vanished from config between `dispatch_ready` and here.
    /// `dispatch_ready` already warned, so the slot is released in silence.
    UnknownWorkflow,
    /// Refused with a reason the operator can act on — the caller turns this
    /// into `fail_dispatch`, which is the only thing that writes.
    Failed(String),
    /// The agent plugin is enabled but **between instances**: it crashed and a
    /// relaunch is booked (#495). The slot is released and the task is left
    /// **queued**, so a later cycle picks it up once the plugin is back
    /// (#499).
    ///
    /// Distinct from [`Failed`](Self::Failed) because the two clocks do not
    /// line up: the automatic dispatch retries are `DISPATCH_RETRY_LIMIT`
    /// attempts one `SETTLE_TICK` apart — under a second — while the first
    /// restart backoff alone is one second. Treating "between instances" as a
    /// dispatch failure spends the whole retry budget and fails the task
    /// before its agent has made a single attempt to come back.
    AgentDown,
}

/// What the agent gate decides for one task (#499).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentGate {
    /// The agent can take the task.
    Proceed,
    /// The agent is between instances and this task has attempted nothing:
    /// leave it queued.
    Park,
    /// Refuse with a reason the operator can act on.
    Refuse(String),
}

/// Whether a task may be dispatched to `status` right now, given how many
/// automatic dispatch retries it has already spent (#499).
///
/// **One definition, two callers.** `dispatch_ready` consults it *before*
/// acquiring a slot, so a parked task does not hold concurrency it cannot use;
/// [`resolve_dispatch_target`] consults it again at the last moment, because
/// the plugin can die in between. Splitting the rule across the two would let
/// them drift, and the drift would be invisible — both sides would look
/// reasonable on their own.
pub(super) fn agent_gate(
    agent_name: &str,
    status: Option<&AgentStatus>,
    spent_retries: u32,
) -> AgentGate {
    let Some(status) = status else {
        return AgentGate::Refuse(format!(
            "agent plugin `{agent_name}` is not launched → install and enable it"
        ));
    };
    if !status.live {
        // Down for good: nothing will bring it back, so parking would park
        // forever. Say why, rather than letting the task discover it through
        // three identical RPC failures.
        if status.abandoned {
            return AgentGate::Refuse(format!(
                "agent plugin `{agent_name}` is down and will not be restarted → fix it and re-run `totsuka run`"
            ));
        }
        // A relaunch is booked, **and this task has attempted nothing**.
        //
        // The second half is what keeps this from swallowing a task whose own
        // dispatch is what kills the plugin (`crash_on_dispatch`): once a task
        // is being counted by the retry budget, that budget must be allowed to
        // run out, or the task would park through restart after restart and
        // never reach a terminal state. A task that has attempted nothing is
        // in a different situation and simply waits.
        if spent_retries == 0 {
            return AgentGate::Park;
        }
    }
    // Checked after liveness: a dead process's declared capabilities are still
    // whatever it declared, so testing them first would report the wrong
    // problem — and report it as a terminal refusal.
    if !status.capabilities.state_stream {
        return AgentGate::Refuse(format!(
            "agent plugin `{agent_name}` does not declare the `state_stream` capability → totsuka cannot track its progress; use a state-streaming agent plugin"
        ));
    }
    AgentGate::Proceed
}

/// What the agent gate needs to know about one agent plugin (#495/#499).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentStatus {
    /// The plugin's declared capabilities (F-33).
    pub capabilities: plugin_protocol::manifest::Capabilities,
    /// Whether the process is running. A crashed plugin stays in the engine's
    /// map until a relaunch replaces it, so **presence is not liveness**.
    pub live: bool,
    /// Whether the supervisor has stopped trying to bring it back (#495): the
    /// restart budget ran out, `restart = false`, or no launch spec was
    /// recorded. Down **and** abandoned means waiting for it is waiting
    /// forever, so such a task must fail rather than park.
    pub abandoned: bool,
}

/// Everything settled before the first side effect: which agent, which
/// repository, which tool (#471).
///
/// Split out of `dispatch_one` so the decisions can be exercised without
/// launching a plugin or creating a worktree. #196's tool precedence and the
/// capability refusals used to be reachable only through a full dispatch.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DispatchTarget {
    /// Agent plugin instance the workflow names.
    pub agent_name: String,
    /// Copied out of the workflow: `wf` borrows `self.settings`, and the launch
    /// spec is assembled inside a closure that outlives that borrow.
    pub profile: Option<crate::config::Profile>,
    /// Already trimmed to `None` when blank (#415).
    pub initial_prompt: Option<String>,
    /// The repository the task was routed to.
    pub repo: RepoSettings,
    /// Resolved tool name (#196: workflow pin > repo default > global default).
    pub tool_name: String,
    /// That tool's profile.
    pub tool_profile: crate::tool::ToolProfile,
}

/// Decide where a task should be dispatched, or why it cannot be (#471).
///
/// **Pure**: no I/O, no `self`. `agent_status` answers "is this agent plugin
/// launched, is its process alive, has the supervisor given up on it, and what
/// does it declare" — a closure rather than the plugin map so a test can
/// answer it with a literal. `spent_retries` is the task's automatic
/// dispatch-retry count (#492), read by the caller because this function does
/// not touch the DB.
///
/// The refusal messages are the ones an operator reads when a task fails, so
/// they are kept verbatim. **The order is not the side-effecting version's any
/// more**: since #499 the liveness checks run *before* the capability check,
/// because a dead process's declared capabilities are still whatever it
/// declared, and reporting those first would name the wrong problem — as a
/// terminal failure. The agent decision itself lives in [`agent_gate`], shared
/// with `dispatch_ready` so the two cannot drift.
pub(super) fn resolve_dispatch_target(
    record: &TaskRecord,
    settings: &EngineSettings,
    agent_status: impl Fn(&str) -> Option<AgentStatus>,
    spent_retries: u32,
) -> Result<DispatchTarget, DispatchRefusal> {
    let repo_name = record.repo.clone().unwrap_or_default();
    let workflows = workflows_by_name(&settings.workflows);
    let Some(wf) = workflows.get(record.workflow.as_str()).copied() else {
        return Err(DispatchRefusal::UnknownWorkflow);
    };
    let agent_name = wf.agent.clone();

    let Some(repo) = settings.repos.iter().find(|r| r.name == repo_name).cloned() else {
        return Err(DispatchRefusal::Failed(format!(
            "selected repository `{repo_name}` is not configured → re-add it to [[repositories]]"
        )));
    };
    match agent_gate(
        &agent_name,
        agent_status(&agent_name).as_ref(),
        spent_retries,
    ) {
        AgentGate::Proceed => {}
        AgentGate::Park => return Err(DispatchRefusal::AgentDown),
        AgentGate::Refuse(reason) => return Err(DispatchRefusal::Failed(reason)),
    }

    // AI-tool resolution (#196): workflow pin > repo default > global default.
    // The registry always contains the built-ins, so an unknown name here is a
    // config-drift error (validation catches it upfront); a kind without an
    // adapter could never signal completion, so both are refused before any
    // side effect (no worktree, no session row).
    let tool_name = crate::tool::resolve_tool_name(
        wf.tool.as_deref(),
        repo.tool.as_deref(),
        &settings.default_tool,
    );
    let Some(tool_profile) = settings.tools.get(&tool_name).cloned() else {
        return Err(DispatchRefusal::Failed(format!(
            "resolved tool `{tool_name}` is not configured → add `[tools.{tool_name}]` or fix the `tool`/`default_tool` reference"
        )));
    };
    if !tool_profile.kind.has_adapter() {
        return Err(DispatchRefusal::Failed(format!(
            "tool `{tool_name}` (kind `{}`) has no completion-detection adapter yet → use a kind with an adapter",
            tool_profile.kind.as_str()
        )));
    }

    Ok(DispatchTarget {
        agent_name,
        profile: wf.profile,
        initial_prompt: wf.initial_prompt.clone(),
        repo,
        tool_name,
        tool_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::workflow::Trigger;

    fn settings(workflows: Vec<Workflow>, repos: Vec<RepoSettings>) -> EngineSettings {
        EngineSettings {
            workflows,
            repos,
            limits: Limits::global(1),
            worktree_name_template: DEFAULT_WORKTREE_NAME_TEMPLATE.to_string(),
            location_template: "/tmp/totsuka-dispatch/{repo_name}/{worktree_name}".to_string(),
            cleanup_implement: CleanupPolicy::Manual,
            cleanup_plan: CleanupPolicy::Immediate,
            env: HashMap::new(),
            select: SelectConfig::default(),
            readme_cache_dir: None,
            worktree_sweep_interval: Duration::ZERO,
            one_shot_grace: Duration::ZERO,
            tools: crate::tool::builtin_registry(),
            default_tool: "claude".to_string(),
            prompts: Default::default(),
            plugin_restart: Default::default(),
            restart_disabled: Default::default(),
            hook: None,
        }
    }

    fn workflow(name: &str, agent: &str, tool: Option<&str>) -> Workflow {
        Workflow {
            name: name.to_string(),
            source: "github".to_string(),
            trigger: Trigger::new(toml::Table::new()),
            mode: WorkflowMode::Implement,
            agent: agent.to_string(),
            output: crate::config::OutputPolicy::None,
            on_success: None,
            on_failure: None,
            verification: crate::config::VerificationMode::None,
            rubric: None,
            timeout_secs: Some(1800),
            tool: tool.map(str::to_string),
            profile: None,
            initial_prompt: None,
            publish: None,
            cleanup: None,
        }
    }

    fn repo(name: &str, tool: Option<&str>) -> RepoSettings {
        RepoSettings {
            name: name.to_string(),
            path: PathBuf::from("/tmp/repo"),
            summary: None,
            tool: tool.map(str::to_string),
            worktree_location: None,
        }
    }

    fn record(workflow: &str, repo: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: 1,
            source: "github".to_string(),
            source_task_id: "1".to_string(),
            workflow: workflow.to_string(),
            mode: "implement".to_string(),
            repo: repo.map(str::to_string),
            worktree_path: None,
            branch: None,
            base_commit: None,
            state: TaskState::Queued,
            priority: 0,
            title: "t".to_string(),
            url: None,
            source_payload: None,
            finished_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            last_signal_at: None,
        }
    }

    /// Every agent this workspace ships streams state; a stub that does too,
    /// and is alive.
    fn streaming(_: &str) -> Option<AgentStatus> {
        Some(AgentStatus {
            capabilities: plugin_protocol::manifest::Capabilities {
                state_stream: true,
                ..Default::default()
            },
            live: true,
            abandoned: false,
        })
    }

    /// The same agent mid-crash: still in the engine's map (no relaunch has
    /// replaced it yet) but its process is gone, and a restart is booked.
    fn crashed(_: &str) -> Option<AgentStatus> {
        Some(AgentStatus {
            live: false,
            ..streaming("").unwrap()
        })
    }

    /// Down, and the supervisor has stopped trying.
    fn abandoned(_: &str) -> Option<AgentStatus> {
        Some(AgentStatus {
            live: false,
            abandoned: true,
            ..streaming("").unwrap()
        })
    }

    /// #499: the four states a dispatch can meet its agent in, and why the
    /// answer differs in each.
    ///
    /// The two clocks are the reason this is not one rule: the automatic
    /// dispatch retries are `DISPATCH_RETRY_LIMIT` attempts one `SETTLE_TICK`
    /// apart — under a second — while the first restart backoff alone is one
    /// second. Without a distinction, a task queued during a crash window
    /// spends its whole budget and reaches terminal `Failed` before the plugin
    /// has made a single attempt to come back.
    #[test]
    fn a_dispatch_meeting_a_downed_agent_waits_only_when_waiting_can_help() {
        let s = settings(
            vec![workflow("implement", "herdr", None)],
            vec![repo("web", None)],
        );
        let task = record("implement", Some("web"));

        // Down, a relaunch is booked, and this task has attempted nothing:
        // wait. Failing here is what #499 was filed for.
        assert_eq!(
            resolve_dispatch_target(&task, &s, crashed, 0),
            Err(DispatchRefusal::AgentDown),
        );

        // Down, but this task has already spent a retry — its own dispatch may
        // be what keeps killing the plugin (`crash_on_dispatch`). The budget
        // must be allowed to run out, or the task parks through restart after
        // restart and never reaches a terminal state.
        assert!(
            resolve_dispatch_target(&task, &s, crashed, 1).is_ok(),
            "a task already being counted by the retry budget must not park",
        );

        // Down for good: waiting is waiting forever, so say so instead.
        let err = resolve_dispatch_target(&task, &s, abandoned, 0).unwrap_err();
        assert!(
            matches!(&err, DispatchRefusal::Failed(m) if m.contains("will not be restarted")),
            "{err:?}",
        );

        // Alive: nothing special happens, whatever the retry count.
        assert!(resolve_dispatch_target(&task, &s, streaming, 0).is_ok());
        assert!(resolve_dispatch_target(&task, &s, streaming, 2).is_ok());
    }

    /// The gate is one function, and `dispatch_ready` consults it **before**
    /// acquiring a slot (#499).
    ///
    /// `plan_dispatch` fixes the plan for a whole cycle, so a slot released
    /// inside `dispatch_one`'s park arm comes back too late to help anyone:
    /// with `max_concurrency = 4`, four tasks queued for a crashed agent would
    /// take every global slot each tick and starve a task for a *healthy*
    /// agent for the entire outage. Before #499 those tasks failed within
    /// ~600 ms and stopped competing, so parking them without moving the gate
    /// would have traded one bug for a worse one.
    ///
    /// This test pins the shared definition; the filter and the resolver both
    /// call it, which is what keeps them from drifting.
    #[test]
    fn the_gate_is_shared_by_the_filter_and_the_resolver() {
        let live = streaming("").unwrap();
        let down = crashed("").unwrap();
        let gone = abandoned("").unwrap();

        assert_eq!(agent_gate("a", Some(&live), 0), AgentGate::Proceed);
        assert_eq!(agent_gate("a", Some(&live), 3), AgentGate::Proceed);
        // Down with nothing attempted: wait, and (in `dispatch_ready`) without
        // taking a slot.
        assert_eq!(agent_gate("a", Some(&down), 0), AgentGate::Park);
        // Down but already being counted: let the budget finish.
        assert_eq!(agent_gate("a", Some(&down), 1), AgentGate::Proceed);
        // Down for good: waiting is waiting forever.
        assert!(matches!(
            agent_gate("a", Some(&gone), 0),
            AgentGate::Refuse(m) if m.contains("will not be restarted")
        ));
        // Not launched at all is a config problem, not an outage.
        assert!(matches!(
            agent_gate("a", None, 0),
            AgentGate::Refuse(m) if m.contains("install and enable")
        ));
        // And the resolver reaches the same answers through the same function.
        let s = settings(
            vec![workflow("implement", "herdr", None)],
            vec![repo("web", None)],
        );
        let task = record("implement", Some("web"));
        assert_eq!(
            resolve_dispatch_target(&task, &s, crashed, 0),
            Err(DispatchRefusal::AgentDown)
        );
        assert!(resolve_dispatch_target(&task, &s, crashed, 1).is_ok());
    }

    /// The liveness gate comes **before** the capability gate on purpose.
    ///
    /// A dead process's declared capabilities are still whatever it declared,
    /// so checking them first reports the wrong problem — and reports it as a
    /// terminal `Failed`, killing a task whose agent was about to return.
    #[test]
    fn liveness_is_judged_before_capabilities() {
        let s = settings(
            vec![workflow("implement", "herdr", None)],
            vec![repo("web", None)],
        );
        let dead_and_streamless = |_: &str| {
            Some(AgentStatus {
                capabilities: plugin_protocol::manifest::Capabilities::default(),
                live: false,
                abandoned: false,
            })
        };
        assert_eq!(
            resolve_dispatch_target(
                &record("implement", Some("web")),
                &s,
                dead_and_streamless,
                0
            ),
            Err(DispatchRefusal::AgentDown),
        );
    }

    /// #196's precedence, without launching a plugin or creating a worktree —
    /// which is the point of splitting the decision out (#471). Before this it
    /// was reachable only through a full dispatch.
    #[test]
    fn tool_resolution_prefers_the_workflow_pin_then_the_repo_then_the_default() {
        let cases = [
            (Some("codex"), Some("opencode"), "codex"),
            (None, Some("opencode"), "opencode"),
            (None, None, "claude"),
        ];
        for (wf_tool, repo_tool, expected) in cases {
            let s = settings(
                vec![workflow("implement", "herdr", wf_tool)],
                vec![repo("web", repo_tool)],
            );
            let target =
                resolve_dispatch_target(&record("implement", Some("web")), &s, streaming, 0)
                    .expect("resolvable");
            assert_eq!(
                target.tool_name, expected,
                "workflow={wf_tool:?} repo={repo_tool:?}"
            );
        }
    }

    /// A workflow that vanished from config is not a task failure: the slot is
    /// released and `dispatch_ready`'s warning stands. Refusing it as `Failed`
    /// would mark a task dead over an edit the operator is mid-way through.
    #[test]
    fn an_unknown_workflow_is_released_not_failed() {
        let s = settings(Vec::new(), vec![repo("web", None)]);
        assert_eq!(
            resolve_dispatch_target(&record("gone", Some("web")), &s, streaming, 0),
            Err(DispatchRefusal::UnknownWorkflow)
        );
    }

    /// Each refusal names what to do about it — these strings are what an
    /// operator reads on a failed task, so they are asserted, not just the
    /// variant.
    #[test]
    fn each_refusal_says_what_to_fix() {
        let with_repo = |name: &str| {
            settings(
                vec![workflow("implement", "herdr", None)],
                vec![repo(name, None)],
            )
        };

        // Repository dropped from `[[repositories]]` after selection.
        let err = resolve_dispatch_target(
            &record("implement", Some("web")),
            &with_repo("other"),
            streaming,
            0,
        )
        .unwrap_err();
        assert!(
            matches!(&err, DispatchRefusal::Failed(m) if m.contains("[[repositories]]")),
            "{err:?}"
        );

        // Plugin not launched.
        let err = resolve_dispatch_target(
            &record("implement", Some("web")),
            &with_repo("web"),
            |_| None,
            0,
        )
        .unwrap_err();
        assert!(
            matches!(&err, DispatchRefusal::Failed(m) if m.contains("install and enable")),
            "{err:?}"
        );

        // Launched but cannot stream state: the task could never progress and
        // would hold its slot for the life of the process.
        let mute = |_: &str| {
            Some(AgentStatus {
                capabilities: plugin_protocol::manifest::Capabilities::default(),
                live: true,
                abandoned: false,
            })
        };
        let err = resolve_dispatch_target(
            &record("implement", Some("web")),
            &with_repo("web"),
            mute,
            0,
        )
        .unwrap_err();
        assert!(
            matches!(&err, DispatchRefusal::Failed(m) if m.contains("state_stream")),
            "{err:?}"
        );

        // A tool the registry does not have.
        let s = settings(
            vec![workflow("implement", "herdr", Some("nosuchtool"))],
            vec![repo("web", None)],
        );
        let err = resolve_dispatch_target(&record("implement", Some("web")), &s, streaming, 0)
            .unwrap_err();
        assert!(
            matches!(&err, DispatchRefusal::Failed(m) if m.contains("[tools.nosuchtool]")),
            "{err:?}"
        );
    }

    /// The decision reads nothing and writes nothing: calling it twice on the
    /// same inputs gives the same answer, which is what lets the caller keep
    /// every side effect (worktree, session row, `fail_dispatch`) to itself.
    #[test]
    fn resolving_twice_gives_the_same_target() {
        let s = settings(
            vec![workflow("implement", "herdr", None)],
            vec![repo("web", Some("codex"))],
        );
        let r = record("implement", Some("web"));
        assert_eq!(
            resolve_dispatch_target(&r, &s, streaming, 0),
            resolve_dispatch_target(&r, &s, streaming, 0)
        );
    }
}
