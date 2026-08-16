//! Getting a queued task in front of an agent (#464).
//!
//! Repository selection (F-10–F-14), slot-gated dispatch (F-40–F-43), and the
//! retry/re-attach path (F-44). The worktree is created here and handed over on
//! a detached `HEAD`; naming the branch is the agent's job (ADR-0026).

use super::*;

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

    /// Dispatch queued tasks with a selected repository, gated by slots
    /// (F-40–F-43).
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

    /// Dispatch a single task: worktree (create or reuse), `task/dispatch` (or
    /// `session/attach` on retry reuse, F-44), `state/subscribe`. The slot has
    /// already been acquired; failure paths release it and fail the task.
    async fn dispatch_one(&mut self, task_id: i64) -> Result<(), EngineError> {
        let record = self
            .db
            .get_task(task_id)?
            .ok_or(StateError::NotFound(task_id))?;
        let repo_name = record.repo.clone().unwrap_or_default();
        let workflows = workflows_by_name(&self.settings.workflows);
        let Some(wf) = workflows.get(record.workflow.as_str()).copied() else {
            self.release_slot(task_id);
            return Ok(()); // warned in dispatch_ready
        };
        let agent_name = wf.agent.clone();
        // Copied out because `wf` borrows `self.settings` and the launch spec
        // is built inside a closure further down. `Option<Profile>` is `Copy`.
        let wf_profile = wf.profile;
        // Same reason; already trimmed to `None` when blank (#415).
        let initial_prompt = wf.initial_prompt.clone();

        let Some(repo) = self
            .settings
            .repos
            .iter()
            .find(|r| r.name == repo_name)
            .cloned()
        else {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "selected repository `{repo_name}` is not configured → re-add it to [[repositories]]"
                    ),
                )
                .await;
        };
        match self.plugins.agents.get(&agent_name) {
            None => {
                return self
                    .fail_dispatch(
                        &record,
                        format!(
                            "agent plugin `{agent_name}` is not launched → install and enable it"
                        ),
                    )
                    .await;
            }
            // Without a state stream the task could never progress and its
            // slot would be held for the life of the process — refuse upfront.
            Some(agent) if !agent.capabilities().state_stream => {
                return self
                    .fail_dispatch(
                        &record,
                        format!(
                            "agent plugin `{agent_name}` does not declare the `state_stream` capability → totsuka cannot track its progress; use a state-streaming agent plugin"
                        ),
                    )
                    .await;
            }
            Some(_) => {}
        }

        // AI-tool resolution (#196): workflow pin > repo default > global
        // default. The registry always contains the built-ins, so an unknown
        // name here is a config-drift error (validation catches it upfront);
        // a kind without an adapter could never signal completion, so both
        // are refused before any side effect (no worktree, no session row).
        let tool_name = crate::tool::resolve_tool_name(
            wf.tool.as_deref(),
            repo.tool.as_deref(),
            &self.settings.default_tool,
        );
        let Some(tool_profile) = self.settings.tools.get(&tool_name).cloned() else {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "resolved tool `{tool_name}` is not configured → add `[tools.{tool_name}]` or fix the `tool`/`default_tool` reference"
                    ),
                )
                .await;
        };
        if !tool_profile.kind.has_adapter() {
            return self
                .fail_dispatch(
                    &record,
                    format!(
                        "tool `{tool_name}` (kind `{}`) has no completion-detection adapter yet → use a kind with an adapter",
                        tool_profile.kind.as_str()
                    ),
                )
                .await;
        }

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
        let worktree_path = match &record.worktree_path {
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
                                &record,
                                format!(
                                    "`{}` is already occupied but is not recorded for this task; \
                                     remove it — `git worktree remove {}`, or the cleanup \
                                     `totsuka doctor` offers, or plain `rm -rf` if it is not a \
                                     worktree at all — and retry",
                                    path.display(),
                                    path.display(),
                                ),
                            )
                            .await;
                    }
                    Err(e) => {
                        return self.fail_dispatch(&record, e.to_string()).await;
                    }
                }
            }
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

        // Hook-capable agents (herdr 0.1.3: `resume_session` / `diagnostics_snapshot`)
        // receive a correlation `job_id` + a [`HookLaunchSpec`] so their Claude
        // Code hooks POST completion signals back (#131/#138). The job id's
        // `session_row` must exist *before* launch — it is injected into the
        // process and echoed by every hook — so the session row is reserved up
        // front and its native id filled in after `task/dispatch` returns.
        // Non-hook agents (orca / mock) take the unchanged path below.
        let hook_capable = self
            .plugins
            .agents
            .get(&agent_name)
            .map(|a| a.capabilities().hook_capable())
            .unwrap_or(false);
        let mut task = task_from_record(&record);
        if let Some(body) = conversation_prompt(&pending) {
            task.body = Some(body);
        }
        let (job_id, hook_spec, reserved_row, visible_hook_context) = match hook_capable
            .then(|| self.hook_launch(&record.workflow))
            .flatten()
        {
            Some((settings_path, mut env)) => {
                let session_row = self.db.reserve_session(record.id, &agent_name)?;
                // Thread continuity (#140): tentatively stamp the resumed
                // Claude session id onto the fresh row so a later follow-up can
                // resume it even before this dispatch's SessionStart hook lands
                // (best-effort resilience). The hook's SessionStart reconciles
                // it against the real id (#138: a `--resume` may legitimately
                // change the id → warn + keep the newest).
                if let Some(sid) = &resume_session_id {
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
                prompt_context.push_str(prompts.marker_self_report());
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
        // Persist the native session id (F-37): fill the reserved hook row, or
        // append a fresh row on the non-hook path.
        match reserved_row {
            Some(row) => self.db.set_session_native_id(row, &dispatched.session_id)?,
            None => {
                self.db
                    .record_session(record.id, &agent_name, &dispatched.session_id)?;
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
            (agent_name.clone(), dispatched.session_id.clone()),
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
                    &record,
                    format!("state/subscribe failed: {e} → dispatch cancelled; fix the agent plugin and `task retry`"),
                )
                .await;
        }
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
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": "dispatch", "reason": reason })),
        )?;
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
