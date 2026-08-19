//! Reacting to what the plugins say (#464).
//!
//! One event at a time: a plugin notification becomes a
//! [`PluginEvent`](super::PluginEvent), which drives the task state machine and
//! fans the result out to the notifiers. Terminal states hand off to
//! [`finalize`](super::finalize).

use super::*;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Handle one plugin event.
    pub(super) async fn on_event(&mut self, event: PluginEvent) -> Result<(), EngineError> {
        match event {
            PluginEvent::State(plugin, note) => {
                let Some(&task_id) = self
                    .sessions
                    .get(&(plugin.clone(), note.session_id.clone()))
                else {
                    tracing::debug!(plugin, session_id = %note.session_id, "notification for unknown session");
                    return Ok(());
                };
                if let Some(chunk) = &note.log_chunk {
                    tracing::debug!(task_id, "agent log: {chunk}");
                    // Accumulate the streamed output; it is the `output = source`
                    // publish artifact (F-07).
                    let buf = self.agent_output.entry(task_id).or_default();
                    buf.push_str(chunk);
                    if !chunk.ends_with('\n') {
                        buf.push('\n');
                    }
                }
                self.apply_agent_state(task_id, &plugin, note.state, note.log_chunk)
                    .await
            }
            PluginEvent::Closed(plugin) => self.on_plugin_closed(&plugin).await,
            // A booked relaunch came due (#495, `run::supervise`).
            PluginEvent::RestartDue(plugin) => self.on_restart_due(&plugin).await,
            // A normalized Claude Code hook signal from the UDS receiver (#136):
            // idempotent record → task resolution → state transition →
            // verification → output (#138, `run::hooks`).
            PluginEvent::HookSignal(signal) => self.on_signal(signal).await,
            // A control-UDS focus request (F-94): resolve, ask the agent
            // plugin, and answer the waiting adapter. A dropped receiver just
            // means the caller gave up (timeout) — not an engine error.
            PluginEvent::Focus { task_id, respond } => {
                let outcome = self.focus_task(task_id).await;
                let _ = respond.send(outcome);
                Ok(())
            }
            // A pushed task (`task/submit`, 0.1.6): persist, ack only after
            // the commit, then run repo selection so the loop's dispatch pass
            // can pick the task up. A persistence error answers the plugin
            // with the retryable INTERNAL_ERROR and still propagates — DB
            // failures are run-fatal, and this path must not be forgiving.
            PluginEvent::TaskSubmit {
                source,
                task,
                respond,
            } => match self.on_task_submit(source, task) {
                Ok(result) => {
                    let _ = respond.send(Ok(result));
                    self.select_repos().await
                }
                Err(e) => {
                    let _ = respond.send(Err(jsonrpc::Error::new(
                        plugin_protocol::error_code::INTERNAL_ERROR,
                        format!("failed to persist task: {e} → retry with backoff"),
                    )));
                    Err(e)
                }
            },
            // `task/lookup` (#242): read-only, so a failure is answered and
            // dropped rather than being run-fatal the way a lost write is.
            // The plugin's fallback — resolve the repository as it always did
            // — is correct behaviour, just more work.
            PluginEvent::TaskLookup {
                source,
                task_id,
                respond,
            } => {
                let answer = self.db.find_by_source(&source, &task_id).map(|found| {
                    // `repo: None` on a known task is a real state, not a
                    // miss: selection has not settled (a human is being
                    // asked, or classification was inconclusive). The plugin
                    // reads it as "no hint", never as "no repository".
                    TaskLookupResult {
                        known: found.is_some(),
                        repo: found.and_then(|t| t.repo),
                    }
                });
                let _ = respond.send(answer.map_err(|e| {
                    jsonrpc::Error::new(
                        plugin_protocol::error_code::INTERNAL_ERROR,
                        format!("task/lookup failed: {e} → resolve without the hint"),
                    )
                }));
                Ok(())
            }
        }
    }

    /// Advance a task's state machine to match the agent's reported state
    /// (F-32), handling slots (F-45), notifier delivery (F-35/F-90), and
    /// terminal processing.
    pub(super) async fn apply_agent_state(
        &mut self,
        task_id: i64,
        agent_plugin: &str,
        state: AgentState,
        log_chunk: Option<String>,
    ) -> Result<(), EngineError> {
        let record = self
            .db
            .get_task(task_id)?
            .ok_or(StateError::NotFound(task_id))?;
        // Only tasks in the agent-driven pipeline react to agent states; a
        // stray/late notification for a queued, pending, or finished task is
        // ignored rather than corrupting the state machine.
        if !matches!(
            record.state,
            TaskState::Dispatched
                | TaskState::Running
                | TaskState::WaitingInput
                | TaskState::Publishing
        ) {
            return Ok(());
        }
        let repo = record.repo.clone().unwrap_or_default();
        let detail = serde_json::json!({ "kind": "agent_state", "state": state });

        match state {
            AgentState::Idle => {}
            AgentState::Running => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start],
                    TaskState::WaitingInput => &[TaskEvent::ResumeInput],
                    _ => &[],
                };
                let resuming = record.state == TaskState::WaitingInput;
                for &event in events {
                    self.db.apply_event(task_id, event, Some(detail.clone()))?;
                }
                if resuming {
                    // F-45: resuming re-acquires a slot. Reality wins over the
                    // cap: the agent *is* running, so a full tier only logs —
                    // but the ledger stays empty, so this task's completion
                    // will not release a slot another task holds.
                    if self.slots.acquire(&repo, agent_plugin) {
                        self.slot_holders
                            .insert(task_id, (repo.clone(), agent_plugin.to_string()));
                    } else {
                        tracing::warn!(
                            task_id,
                            "resumed task exceeds a concurrency cap temporarily"
                        );
                    }
                }
            }
            AgentState::WaitingInput => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start, TaskEvent::WaitInput],
                    TaskState::Running => &[TaskEvent::WaitInput],
                    _ => &[],
                };
                if !events.is_empty() {
                    for &event in events {
                        self.db.apply_event(task_id, event, Some(detail.clone()))?;
                    }
                    // F-45: a waiting task frees its slot.
                    self.release_slot(task_id);
                    tracing::info!(task_id, "agent is waiting for input (F-35)");
                    notify_all(
                        &self.plugins.notifiers,
                        NotifierEvent::WaitingInput,
                        &record,
                        log_chunk,
                    );
                }
            }
            AgentState::Done => {
                let events: &[TaskEvent] = match record.state {
                    TaskState::Dispatched => &[TaskEvent::Start, TaskEvent::BeginPublish],
                    TaskState::Running => &[TaskEvent::BeginPublish],
                    TaskState::WaitingInput => &[TaskEvent::ResumeInput, TaskEvent::BeginPublish],
                    _ => &[],
                };
                for &event in events {
                    // Persist the accumulated agent output on the BeginPublish
                    // transition, so a crash before finalize can recover the
                    // artifact from the audit log (source publish / PR summary).
                    let event_detail = if event == TaskEvent::BeginPublish {
                        serde_json::json!({
                            "kind": "agent_state", "state": state,
                            "publish_artifact": self.agent_output.get(&task_id),
                        })
                    } else {
                        detail.clone()
                    };
                    self.db.apply_event(task_id, event, Some(event_detail))?;
                }
                self.finalize_success(&record).await?;
            }
            AgentState::Failed => {
                self.db.apply_event(
                    task_id,
                    TaskEvent::Fail,
                    Some(serde_json::json!({ "kind": "agent_state", "state": "failed" })),
                )?;
                self.release_slot(task_id);
                self.drop_task_sessions(task_id);
                self.agent_output.remove(&task_id);
                self.stats.failed += 1;
                self.write_back_status(&record, false).await;
                // The worktree is kept for `task retry` (F-44).
                notify_all(
                    &self.plugins.notifiers,
                    NotifierEvent::Failed,
                    &record,
                    log_chunk,
                );
                tracing::warn!(task_id, "task failed");
            }
        }
        Ok(())
    }
}
