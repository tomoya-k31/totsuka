//! Turning a pushed `task/submit` into a row (#464).
//!
//! Idempotent by the conversation ledger's `message_key` (F-73), which is what
//! lets every source re-deliver freely — `plugin_sdk::poll_loop` has no dedup
//! of its own and re-submits everything each tick.

use super::*;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Persist one normalized task under `wf`, idempotently (F-73), appending
    /// the delivery to the conversation's message ledger. Returns
    /// `(row id, outcome)`. Every ingest since 0.2.0 arrives via `task/submit`
    /// (see [`Self::on_task_submit`]).
    ///
    /// Since #242 a task is a **conversation**, so `source_task_id` no longer
    /// distinguishes deliveries — every message in a Slack thread carries the
    /// same one. Dedup therefore moves to the ledger's `message_key`, and the
    /// task row's `ON CONFLICT DO NOTHING` goes back to meaning only "this
    /// conversation already exists" rather than "drop this message".
    fn ingest_task(
        &mut self,
        wf: &Workflow,
        task: &Task,
    ) -> Result<(i64, IngestOutcome), EngineError> {
        let existing = self.db.find_by_source(&task.source, &task.id)?;
        // A delivery under a **different workflow** hands the conversation
        // over to it (#565), when the conversation has finished. This is what
        // makes a column pipeline work: design finishes, its write-back puts
        // the card in the implement column, and that lane entry (#556) arrives
        // as a new message under the implement workflow — the same
        // conversation continues there, keeping its worktree and its agent
        // session. A conversation still in flight is left alone; see
        // `append_task_message_handing_off`.
        if let Some(existing) = &existing
            && existing.workflow != wf.name
        {
            return self.hand_off_workflow(existing, wf, task);
        }
        let new_task = NewTask {
            source: task.source.clone(),
            source_task_id: task.id.clone(),
            workflow: wf.name.clone(),
            mode: mode_str(wf.mode).to_string(),
            repo: None,
            priority: task.priority,
            title: task.title.clone(),
            url: task.url.clone(),
            // The full normalized task, so dispatch can reconstruct it.
            source_payload: serde_json::to_value(task).ok(),
            // The R-10 timeout anchor starts unset; the first hook signal
            // stamps it.
            last_signal_at: None,
        };
        // Existing conversations are left untouched by this (`DO NOTHING`):
        // the title stays the one the conversation opened with, which is what
        // a thread's subject should be.
        let id = self.db.upsert_submitted_task(&new_task)?;

        // A source that sends one message per task omits `message_key`;
        // falling back to the conversation id makes its second delivery
        // collide exactly as before, so GitHub and Notion keep their old
        // at-most-once behaviour without knowing this field exists.
        let message_key = task.message_key.clone().unwrap_or_else(|| task.id.clone());
        let insert = TaskMessageInsert {
            task_id: id,
            message_key: message_key.clone(),
            // The normalized schema has no author field; whoever the source
            // named is preserved in `payload`. Left NULL rather than filled
            // with `assignee`, which is a different person in every source.
            author: None,
            body: task.body.clone().unwrap_or_default(),
            url: task.url.clone(),
            // Not `unwrap_or_default()`: an empty payload would silently
            // destroy the audit record (N-01) and anything reading the message
            // back. A failure here is a persistence failure like any other.
            payload: serde_json::to_string(task).map_err(StateError::from)?,
        };
        // Appending and reopening are one transaction: see
        // `append_task_message_reopening` for why splitting them can strand a
        // message forever.
        let (appended, reopened) = self.db.append_task_message_reopening(
            &insert,
            Some(serde_json::json!({ "kind": "reopen", "message_key": message_key })),
        )?;

        let outcome = match (existing, appended, reopened) {
            (None, ..) => IngestOutcome::Created,
            // The ledger already had this delivery: a Socket Mode re-send, or
            // a restart replaying an unacked message. This is the last line of
            // defence — plugin-side dedup lives in memory and dies with the
            // process — so it must change nothing at all.
            (Some(_), TaskMessageOutcome::Duplicate, _) => IngestOutcome::Duplicate,
            (Some(_), TaskMessageOutcome::New, Some(_)) => IngestOutcome::Reopened,
            // Still in flight: the message waits in the ledger and the
            // in-progress dispatch picks it up when it finishes.
            (Some(_), TaskMessageOutcome::New, None) => IngestOutcome::Appended,
        };
        Ok((id, outcome))
    }

    /// Move a finished conversation to the workflow a delivery arrived under
    /// (#565), or leave it alone while it is still running.
    ///
    /// The two outcomes are deliberately asymmetric. A finished conversation
    /// is **reopened under the new workflow** — that is the column pipeline
    /// working. A running one is **dropped without touching the ledger**: the
    /// stage in flight owns the conversation until it ends. Switching mid-run
    /// would need a cancel, and cancelling is a person's decision — an
    /// operator moving a card must not silently abort an agent.
    ///
    /// **Dropping it unwritten only recovers on a source that re-delivers.**
    /// A poller does (`plugin_sdk::poll_loop` keeps no seen-set, so the next
    /// tick brings the same lane entry back and the handoff happens then), and
    /// that is the case column pipelines are built on. A push source that acks
    /// first does **not**: Slack's Socket Mode envelope is acked before the
    /// work, so a cross-workflow trigger that arrives while the conversation
    /// is running is lost — only the `warn!` below records it. Writing the
    /// message instead would be worse, not better: it would strand the
    /// delivery on *every* source, permanently, because the ledger would then
    /// dedup the re-delivery the poller was about to send.
    ///
    /// A conversation whose deliveries carry no `message_key` (a label-only
    /// GitHub trigger, a source that sends one message per task) can never
    /// reach the handoff: its key falls back to the conversation id, which the
    /// ledger already holds, so the delivery is a `Duplicate`. That is the same
    /// limit lane re-entry has (#556) and it is the same reason — without a key
    /// per lane entry there is no way to tell a re-entry from a re-delivery.
    fn hand_off_workflow(
        &mut self,
        existing: &TaskRecord,
        wf: &Workflow,
        task: &Task,
    ) -> Result<(i64, IngestOutcome), EngineError> {
        let message_key = task.message_key.clone().unwrap_or_else(|| task.id.clone());
        let insert = TaskMessageInsert {
            task_id: existing.id,
            message_key: message_key.clone(),
            author: None,
            body: task.body.clone().unwrap_or_default(),
            url: task.url.clone(),
            payload: serde_json::to_string(task).map_err(StateError::from)?,
        };
        // `?`, not `.ok()`: this value **overwrites** the row's existing
        // `source_payload`, so swallowing the error would replace the previous
        // stage's payload with NULL and leave the next dispatch rebuilding the
        // task from nothing. On the create path a `None` merely means a fresh
        // row has no payload yet, which is why that one may be lenient.
        let payload = serde_json::to_value(task).map_err(StateError::from)?;
        let detail = serde_json::json!({
            "kind": "reopen",
            "cause": "workflow_handoff",
            "workflow": { "from": existing.workflow, "to": wf.name },
            "message_key": message_key,
        });
        let outcome = self.db.append_task_message_handing_off(
            &insert,
            &wf.name,
            mode_str(wf.mode),
            Some(&payload),
            Some(detail),
        )?;
        match outcome {
            HandoffOutcome::HandedOff => {
                tracing::info!(
                    task_id = existing.id,
                    from = %existing.workflow,
                    to = %wf.name,
                    "conversation handed over to the delivering workflow"
                );
                self.detach_for_read_only_stage(existing, wf);
                Ok((existing.id, IngestOutcome::Reopened))
            }
            HandoffOutcome::InFlight => {
                tracing::warn!(
                    task_id = existing.id,
                    have = %existing.workflow,
                    delivered = %wf.name,
                    state = %existing.state,
                    "cross-workflow delivery ignored while the conversation is running: \
                     the stage in flight keeps it until it finishes. A polling source \
                     re-delivers, so the handoff happens on a later tick; a source that \
                     acks first (Slack) does not, so this trigger is lost — re-issue it \
                     once the run has finished"
                );
                Ok((existing.id, IngestOutcome::Duplicate))
            }
            HandoffOutcome::Duplicate => Ok((existing.id, IngestOutcome::Duplicate)),
        }
    }

    /// Put an inherited worktree back on a detached `HEAD` when the stage it
    /// was just handed to is read-only (#568).
    ///
    /// Every worktree is created detached, and the read-only check
    /// (ADR-0045) reads "on a branch" as "the agent ran git" **because** of
    /// that. A handoff (#565) keeps the worktree, so a read-only stage
    /// inheriting one from an implement stage inherits its branch too — and
    /// the check then fails it, closes its pane to stop a push it never
    /// intended, and names it for a branch it never made. Observed live: the
    /// pane was killed three seconds in, while the agent was waiting on a
    /// question nobody got to read.
    ///
    /// Detaching restores the invariant instead of teaching the check about
    /// handoffs, so a branch the read-only stage *does* make is still caught
    /// and still stopped hard, which is the behaviour that check is for.
    ///
    /// Nothing is lost: the previous stage's branch ref still points at the
    /// same commit, and the files in the worktree do not move.
    fn detach_for_read_only_stage(&mut self, existing: &TaskRecord, wf: &Workflow) {
        if !wf.profile.is_some_and(|p| p.is_read_only()) {
            return;
        }
        // Forget the branch first, and unconditionally: `acquire_worktree`
        // re-creates a missing worktree **from this column** and puts it back
        // on that branch, so a worktree that cleanup already removed would
        // hand the read-only stage a branch it never made — the same failure
        // the detach below avoids, reached without ever touching git.
        if let Err(e) = self.db.clear_branch(existing.id) {
            tracing::warn!(
                task_id = existing.id,
                "could not forget the inherited branch before a read-only stage: {e}"
            );
        }
        let Some(path) = existing
            .worktree_path
            .as_deref()
            .filter(|p| Path::new(p).is_dir())
        else {
            // Nothing on disk to detach. Cleared above, so re-creation hands
            // over a detached worktree; nothing further to do.
            return;
        };
        let path = Path::new(path);
        // `head_branch` answers `None` for a detached worktree **and** for a
        // git that could not be read, and those must not share an exit here:
        // treating a failed read as "already detached" is how the false
        // positive comes back. Detach unconditionally — it is idempotent, so
        // the ordinary already-detached case costs one no-op git call — and
        // use the branch only to say *what* was detached.
        let branch = self.worktrees.head_branch(path);
        if self.worktrees.detach(path) {
            if let Some(branch) = branch {
                tracing::info!(
                    task_id = existing.id,
                    workflow = %wf.name,
                    branch = %branch,
                    "detached the inherited worktree for a read-only stage (the branch is kept)"
                );
            }
        } else {
            let branch = branch.unwrap_or_else(|| "<unreadable>".to_string());
            // Loud, and it names what happens next: the read-only check will
            // fail this task and close its pane for `branch`, which the new
            // stage did not create.
            tracing::error!(
                task_id = existing.id,
                workflow = %wf.name,
                branch = %branch,
                "could not detach the inherited worktree for a read-only stage → the \
                 read-only check will fail this task and close its pane for a branch the \
                 stage did not make; detach it by hand and `totsuka task retry`"
            );
        }
    }

    /// Ingest one pushed task (`task/submit`, 0.1.6): normalize and resolve
    /// the workflow the plugin named. Returns the final ack; `Err` is a
    /// persistence failure (retryable for the plugin, fatal for the run).
    ///
    /// # Why the plugin names the workflow
    ///
    /// The plugin ran first-match over the workflows it was given at
    /// `initialize`, so it already knows. Until 0.6.0 the Orchestrator
    /// re-derived the same answer from `task.status` / `task.labels` — fields
    /// this very plugin had just filled in. That check could not catch a
    /// wrong plugin (it was reading the plugin's own report), and it forced
    /// every trigger key a source wanted into the Orchestrator's vocabulary:
    /// `reaction` is Slack's word and `project_status` is GitHub Projects',
    /// and both sat in `domain::workflow` (#554).
    ///
    /// What the Orchestrator checks is what it alone knows: that the named
    /// workflow exists, and that it belongs to the plugin that submitted.
    pub(super) fn on_task_submit(
        &mut self,
        source: String,
        workflow: String,
        mut task: Task,
    ) -> Result<TaskSubmitResult, EngineError> {
        // The ingest key uses the `[plugins.<name>]` key, not the plugin's own
        // notion of its source name.
        task.source = source;
        let workflows = self.settings.workflows.clone();
        let Some(wf) = workflows.iter().find(|w| w.name == workflow) else {
            return Ok(TaskSubmitResult {
                status: TaskSubmitStatus::Rejected,
                reason: Some(format!(
                    "no workflow named `{workflow}` → add a [[workflows]] entry \
                     with that name, or correct the name the plugin submits"
                )),
            });
        };
        if wf.source != task.source {
            return Ok(TaskSubmitResult {
                status: TaskSubmitStatus::Rejected,
                reason: Some(format!(
                    "workflow `{workflow}` has source = `{}`, but `{}` submitted \
                     the task → a plugin may only submit to its own workflows",
                    wf.source, task.source
                )),
            });
        }
        let (id, outcome) = self.ingest_task(wf, &task)?;
        match outcome {
            IngestOutcome::Created => {
                self.stats.submitted += 1;
                tracing::info!(task_id = id, workflow = %wf.name, title = %task.title, "task submitted");
            }
            IngestOutcome::Reopened => {
                tracing::info!(
                    task_id = id, workflow = %wf.name,
                    "conversation reopened by a new message"
                );
            }
            IngestOutcome::Appended => {
                tracing::info!(
                    task_id = id, workflow = %wf.name,
                    "message appended to a conversation still in progress"
                );
            }
            IngestOutcome::Duplicate => {
                tracing::debug!(task_id = id, "message already ingested; dropped");
            }
        }
        Ok(TaskSubmitResult {
            status: outcome.ack(),
            reason: None,
        })
    }
}

/// The per-plugin in-flight budgets, one per plugin-initiated method.
pub(super) struct PluginRequestBudgets {
    pub(super) submit: Arc<Semaphore>,
    pub(super) lookup: Arc<Semaphore>,
}

/// Route one plugin-initiated request (P→O) to the engine loop.
///
/// `task/submit` (0.1.6) and `task/lookup` (0.2.4) are parsed and budgeted
/// here; anything else is answered `METHOD_NOT_FOUND` immediately. The answer
/// await is spawned off the caller's loop so one slow ingest never delays the
/// next request's parsing (per-source ordering is already fixed by the inline
/// event-channel send).
pub(super) fn forward_plugin_request(
    source: &str,
    request: IncomingRequest,
    tx: &mpsc::UnboundedSender<PluginEvent>,
    budgets: &PluginRequestBudgets,
) {
    use plugin_protocol::error_code;
    match request.method.as_str() {
        method::TASK_SUBMIT => forward_submit(source, request, tx, &budgets.submit),
        method::TASK_LOOKUP => forward_lookup(source, request, tx, &budgets.lookup),
        other => request.responder.err(jsonrpc::Error::new(
            error_code::METHOD_NOT_FOUND,
            format!("unknown plugin-initiated method: {other}"),
        )),
    }
}

/// Answer `task/lookup` (P→O, 0.2.4, #242) from the engine loop.
///
/// The plugin asks this *before* submitting, to skip work only a new
/// conversation needs — repository resolution, which may mean an LLM call or
/// a question put to a human. It is read-only, so a failure here costs the
/// plugin nothing beyond falling back to resolving as it always did; that
/// degradation is part of the contract, and the client enforces a timeout for
/// exactly this reason (the engine loop can be busy creating a worktree).
fn forward_lookup(
    source: &str,
    request: IncomingRequest,
    tx: &mpsc::UnboundedSender<PluginEvent>,
    budget: &Arc<Semaphore>,
) {
    use plugin_protocol::error_code;
    let params: TaskLookupParams = match request
        .params
        .clone()
        .map(serde_json::from_value)
        .transpose()
    {
        Ok(Some(params)) => params,
        Ok(None) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                "task/lookup requires params → send { \"source\": …, \"task_id\": … }",
            ));
            return;
        }
        Err(e) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                format!("malformed task/lookup params: {e}"),
            ));
            return;
        }
    };
    let Ok(permit) = budget.clone().try_acquire_owned() else {
        request.responder.err(jsonrpc::Error::new(
            error_code::SUBMIT_OVERLOADED,
            "task/lookup in-flight budget exhausted → resolve without the hint",
        ));
        return;
    };
    let (otx, orx) = tokio::sync::oneshot::channel();
    // `source` comes from the connection, not from `params.source`: a plugin
    // must not be able to read another source's conversations by naming it.
    if tx
        .send(PluginEvent::TaskLookup {
            source: source.to_string(),
            task_id: params.task_id,
            respond: otx,
        })
        .is_err()
    {
        request.responder.err(jsonrpc::Error::new(
            error_code::NOT_ACCEPTING,
            "orchestrator is not accepting requests → resolve without the hint",
        ));
        return;
    }
    let responder = request.responder;
    tokio::spawn(async move {
        let _permit = permit;
        match orx.await {
            Ok(Ok(result)) => match serde_json::to_value(&result) {
                Ok(value) => responder.ok(value),
                Err(e) => responder.err(jsonrpc::Error::new(
                    error_code::INTERNAL_ERROR,
                    format!("failed to encode task/lookup answer: {e}"),
                )),
            },
            Ok(Err(error)) => responder.err(error),
            Err(_) => responder.err(jsonrpc::Error::new(
                error_code::NOT_ACCEPTING,
                "orchestrator is shutting down → resolve without the hint",
            )),
        }
    });
}

/// Route one `task/submit` to the engine loop (0.1.6).
fn forward_submit(
    source: &str,
    request: IncomingRequest,
    tx: &mpsc::UnboundedSender<PluginEvent>,
    budget: &Arc<Semaphore>,
) {
    use plugin_protocol::error_code;
    let params: TaskSubmitParams = match request
        .params
        .clone()
        .map(serde_json::from_value)
        .transpose()
    {
        Ok(Some(params)) => params,
        Ok(None) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                "task/submit requires params → send { \"task\": { … }, \"workflow\": \"…\" }",
            ));
            return;
        }
        Err(e) => {
            request.responder.err(jsonrpc::Error::new(
                error_code::INVALID_PARAMS,
                format!("malformed task/submit params: {e}"),
            ));
            return;
        }
    };
    // Backpressure: a bounded in-flight budget per plugin. Exhaustion is the
    // retryable SUBMIT_OVERLOADED, never a dropped request.
    let Ok(permit) = budget.clone().try_acquire_owned() else {
        request.responder.err(jsonrpc::Error::new(
            error_code::SUBMIT_OVERLOADED,
            "task/submit in-flight budget exhausted → retry with backoff",
        ));
        return;
    };
    let (otx, orx) = tokio::sync::oneshot::channel();
    if tx
        .send(PluginEvent::TaskSubmit {
            source: source.to_string(),
            workflow: params.workflow,
            task: params.task,
            respond: otx,
        })
        .is_err()
    {
        request.responder.err(jsonrpc::Error::new(
            error_code::NOT_ACCEPTING,
            "orchestrator is not accepting submissions → retry with backoff",
        ));
        return;
    }
    let responder = request.responder;
    tokio::spawn(async move {
        let _permit = permit;
        match orx.await {
            Ok(Ok(result)) => match serde_json::to_value(&result) {
                Ok(value) => responder.ok(value),
                Err(e) => responder.err(jsonrpc::Error::new(
                    error_code::INTERNAL_ERROR,
                    format!("failed to encode task/submit ack: {e}"),
                )),
            },
            Ok(Err(error)) => responder.err(error),
            // The engine dropped the responder without answering: it is
            // shutting down before persisting. Retryable, never final.
            Err(_) => responder.err(jsonrpc::Error::new(
                error_code::NOT_ACCEPTING,
                "orchestrator is shutting down → retry with backoff",
            )),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine with one workflow named `implement`, which is the name every
    /// test below submits under — so `on_task_submit` resolves and the ingest
    /// path is what the test observes.
    async fn ingest_test_engine() -> Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter> {
        let mut engine = test_engine(Duration::from_secs(3600)).await;
        engine.settings.workflows = vec![Workflow {
            name: "implement".to_string(),
            source: "slack".to_string(),
            trigger: crate::domain::workflow::Trigger::new(toml::Table::new()),
            mode: WorkflowMode::Implement,
            agent: "mock_agent".to_string(),
            output: crate::config::OutputPolicy::None,
            on_start: None,
            on_success: None,
            on_failure: None,
            verification: crate::config::VerificationMode::None,
            timeout_secs: None,
            rubric: None,
            tool: None,
            profile: None,
            initial_prompt: None,
            cleanup: None,
        }];
        engine
    }

    /// One delivery in the conversation `conv`, identified by `message_key`.
    fn delivery(conv: &str, message_key: Option<&str>, body: &str) -> Task {
        Task {
            id: conv.to_string(),
            source: "slack".into(),
            title: "会話".into(),
            body: Some(body.to_string()),
            repo_hint: None,
            labels: vec![],
            priority: 0,
            status: None,
            url: None,
            assignee: None,
            message_key: message_key.map(str::to_string),
            instructions: None,
        }
    }

    /// The whole point of #242: a second message in the same conversation is
    /// accepted rather than silently dropped, and a finished conversation goes
    /// back to work.
    #[tokio::test]
    async fn a_second_message_reopens_a_finished_conversation() {
        let mut engine = ingest_test_engine().await;

        let ack = engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;

        // Drive it to a terminal state.
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done
        );

        // A new message in the same thread.
        let ack = engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:300"), "two"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        let record = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(record.state, TaskState::Queued, "the conversation reopened");
        assert_eq!(
            engine.db.list_tasks().unwrap().len(),
            1,
            "a reply is the same task, not a new one"
        );
        // Both messages are in the ledger, and the new one is queued for the
        // next dispatch while the first stays marked as delivered-or-not on
        // its own terms (nothing has dispatched yet here, so both are pending).
        let messages = engine.db.list_task_messages(id).unwrap();
        assert_eq!(
            messages.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
    }

    /// Add a second workflow on the same source, differing in the fields the
    /// handoff has to move.
    fn add_second_workflow(
        engine: &mut Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter>,
    ) {
        let mut second = engine.settings.workflows[0].clone();
        second.name = "review".to_string();
        second.mode = WorkflowMode::Plan;
        engine.settings.workflows.push(second);
    }

    /// The column pipeline (#565): a finished conversation delivered under
    /// another workflow moves to it, keeping its identity. The three columns
    /// that carry the stage — `workflow`, `mode`, `source_payload` — all move
    /// together, because dispatch reads `mode` and rebuilds the task from
    /// `source_payload` rather than re-resolving either from the workflow.
    #[tokio::test]
    async fn a_finished_conversation_is_handed_over_to_the_delivering_workflow() {
        let mut engine = ingest_test_engine().await;
        add_second_workflow(&mut engine);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // Finish it: only a terminal conversation is handed over.
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        let ack = engine
            .on_task_submit(
                "slack".into(),
                "review".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted, "handed over");

        let record = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(record.workflow, "review", "the stage moved");
        assert_eq!(
            record.mode, "plan",
            "dispatch reads this column, not the workflow"
        );
        assert_eq!(record.state, TaskState::Queued, "reopened");
        // The dispatched task is rebuilt from `source_payload`, so it has to
        // be the delivery that arrived — not the one the row was created with.
        let payload = record.source_payload.expect("payload");
        assert_eq!(payload["body"], "two", "the new stage gets the new body");
        assert_eq!(
            engine.db.list_task_messages(id).unwrap().len(),
            2,
            "both deliveries are in the ledger"
        );
        // The audit trail names the move: `events` records only from/to state,
        // so the provenance has to live in the detail (same rule as Retry).
        let events = engine.db.list_events(id).unwrap();
        let handoff = events
            .iter()
            .filter_map(|e| e.detail.as_ref())
            .find(|d| d["cause"] == "workflow_handoff")
            .expect("the handoff is auditable");
        assert_eq!(handoff["workflow"]["from"], "implement");
        assert_eq!(handoff["workflow"]["to"], "review");
    }

    /// A conversation still in flight keeps its stage, and — the part that
    /// matters — **nothing is written**. Writing the message would make every
    /// re-delivery dedup against it while the row kept the old workflow, so
    /// the handoff could never happen at all.
    #[tokio::test]
    async fn a_running_conversation_is_not_handed_over_and_nothing_is_written() {
        let mut engine = ingest_test_engine().await;
        add_second_workflow(&mut engine);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // Still queued (non-terminal), which is what the guard turns on.
        let ack = engine
            .on_task_submit(
                "slack".into(),
                "review".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();
        assert_eq!(
            ack.status,
            TaskSubmitStatus::Duplicate,
            "ignored, final ack"
        );

        let record = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(record.workflow, "implement", "the running stage keeps it");
        assert_eq!(record.mode, "implement");
        assert_eq!(
            engine.db.list_task_messages(id).unwrap().len(),
            1,
            "the ledger is untouched, so a later re-delivery can still hand it over"
        );

        // And once it finishes, the very same delivery does hand it over —
        // this is why dropping it unwritten is safe.
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();
        let ack = engine
            .on_task_submit(
                "slack".into(),
                "review".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().workflow,
            "review",
            "the re-delivery landed once the stage ended"
        );
    }

    /// Handing a worktree to a **read-only** stage detaches it first (#568).
    ///
    /// Without this the inherited branch trips the read-only check on the
    /// first sweep after dispatch: the task is failed and its pane is closed
    /// to stop a push it never intended — observed live, three seconds in,
    /// while the agent was waiting on a question nobody got to read. Real git
    /// here, because what is being asserted is what git did to `HEAD`.
    #[tokio::test]
    async fn a_read_only_stage_inherits_a_detached_worktree() {
        let base = test_support::scratch("handoff_detach");
        let repo = test_support::bare_origin_and_clone(&base);
        let git = crate::adapters::git::SystemGitRunner;
        let wt = crate::worktree::WorktreeManager::new(crate::adapters::git::SystemGitRunner);
        for args in [
            &["switch", "-c", "feat/prev"][..],
            &["commit", "--allow-empty", "-m", "implement stage"][..],
        ] {
            assert!(
                crate::ports::git::GitRunner::run(&git, &repo, args)
                    .unwrap()
                    .success(),
                "{args:?}"
            );
        }
        assert_eq!(wt.head_branch(&repo).as_deref(), Some("feat/prev"));

        let mut engine = ingest_test_engine().await;
        // The next stage is read-only (design); the current one is not.
        let mut design = engine.settings.workflows[0].clone();
        design.name = "design".to_string();
        design.mode = WorkflowMode::Plan;
        design.profile = Some(crate::config::Profile::Design);
        engine.settings.workflows.push(design);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // Give the row the branched worktree the previous stage left behind.
        engine
            .db
            .set_worktree(id, repo.to_str().unwrap(), Some("feat/prev"), "HEAD")
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        engine
            .on_task_submit(
                "slack".into(),
                "design".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();

        assert_eq!(engine.db.get_task(id).unwrap().unwrap().workflow, "design");
        assert!(
            wt.head_branch(&repo).is_none(),
            "the read-only stage must start detached, or it is blamed for `feat/prev`"
        );
        assert!(
            engine.db.get_task(id).unwrap().unwrap().branch.is_none(),
            "and the column is cleared, so a re-creation cannot put it back on the branch"
        );
        // And the previous stage's work is still reachable by name.
        assert!(
            crate::ports::git::GitRunner::run(&git, &repo, &["rev-parse", "feat/prev"])
                .unwrap()
                .success(),
            "the branch ref survives"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A read-only stage is detached even when the branch cannot be read
    /// (#568 review). `head_branch` answers `None` both for an
    /// already-detached worktree and for a git it could not run, so branching
    /// the decision on it would skip the detach exactly when the state is
    /// unknown — which is when the false positive comes back. Pinned by
    /// handing over a worktree whose `HEAD` git cannot resolve (an empty
    /// repository with no commits): the detach still runs.
    #[tokio::test]
    async fn an_unreadable_head_still_gets_a_detach_attempt() {
        let base = test_support::scratch("handoff_unreadable");
        let repo = base.join("empty");
        std::fs::create_dir_all(&repo).unwrap();
        let git = crate::adapters::git::SystemGitRunner;
        assert!(
            crate::ports::git::GitRunner::run(&git, &repo, &["init", "-q"])
                .unwrap()
                .success()
        );
        let wt = crate::worktree::WorktreeManager::new(crate::adapters::git::SystemGitRunner);
        // No commits: `rev-parse --abbrev-ref HEAD` fails, so this is the
        // "cannot read" flavour of `None`, not the detached one.
        assert!(wt.head_branch(&repo).is_none());

        let mut engine = ingest_test_engine().await;
        let mut design = engine.settings.workflows[0].clone();
        design.name = "design".to_string();
        design.mode = WorkflowMode::Plan;
        design.profile = Some(crate::config::Profile::Design);
        engine.settings.workflows.push(design);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine
            .db
            .set_worktree(id, repo.to_str().unwrap(), None, "HEAD")
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        // The handoff still completes — the detach is best-effort and its
        // failure is logged, never fatal to the ingest.
        let ack = engine
            .on_task_submit(
                "slack".into(),
                "design".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        let row = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(row.workflow, "design");
        // The assertion that actually pins the behaviour: the read-only path
        // ran and forgot the branch. Asserting only the ack would stay green
        // if the `head_branch().is_none()` early return came back.
        assert!(
            row.branch.is_none(),
            "the read-only path must run even when HEAD cannot be read"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The worktree is gone from disk (cleanup removed it, or an operator
    /// did): there is nothing to detach, but `acquire_worktree` re-creates it
    /// **from `tasks.branch`** and would put the read-only stage back on the
    /// previous stage's branch. Clearing the column is what makes re-creation
    /// hand over a detached worktree instead (#568 review).
    #[tokio::test]
    async fn a_removed_worktree_still_loses_its_branch_for_a_read_only_stage() {
        let mut engine = ingest_test_engine().await;
        let mut design = engine.settings.workflows[0].clone();
        design.name = "design".to_string();
        design.mode = WorkflowMode::Plan;
        design.profile = Some(crate::config::Profile::Design);
        engine.settings.workflows.push(design);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // Recorded, but not on disk — exactly what `cleanup = immediate` or an
        // elapsed retention leaves behind.
        engine
            .db
            .set_worktree(id, "/nonexistent/wt", Some("feat/prev"), "HEAD")
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        engine
            .on_task_submit(
                "slack".into(),
                "design".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();

        let row = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(row.workflow, "design");
        assert!(
            row.branch.is_none(),
            "re-creation must hand the read-only stage a detached worktree"
        );
    }

    /// Handing to a stage that is **not** read-only leaves the worktree on its
    /// branch: that is the pipeline continuing its own work, and detaching
    /// would make the next stage start over.
    #[tokio::test]
    async fn a_writing_stage_keeps_the_inherited_branch() {
        let base = test_support::scratch("handoff_keep");
        let repo = test_support::bare_origin_and_clone(&base);
        let git = crate::adapters::git::SystemGitRunner;
        let wt = crate::worktree::WorktreeManager::new(crate::adapters::git::SystemGitRunner);
        for args in [
            &["switch", "-c", "feat/prev"][..],
            &["commit", "--allow-empty", "-m", "stage one"][..],
        ] {
            assert!(
                crate::ports::git::GitRunner::run(&git, &repo, args)
                    .unwrap()
                    .success(),
                "{args:?}"
            );
        }

        let mut engine = ingest_test_engine().await;
        let mut second = engine.settings.workflows[0].clone();
        second.name = "more-implement".to_string();
        second.profile = Some(crate::config::Profile::Implement);
        engine.settings.workflows.push(second);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine
            .db
            .set_worktree(id, repo.to_str().unwrap(), Some("feat/prev"), "HEAD")
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        engine
            .on_task_submit(
                "slack".into(),
                "more-implement".into(),
                delivery("C1:100", Some("k2"), "two"),
            )
            .unwrap();

        assert_eq!(
            wt.head_branch(&repo).as_deref(),
            Some("feat/prev"),
            "a writing stage continues on the branch it inherited"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A re-delivery of a key the ledger already holds changes nothing, even
    /// across workflows: the handoff is driven by *new* messages only.
    #[tokio::test]
    async fn a_duplicate_key_under_another_workflow_changes_nothing() {
        let mut engine = ingest_test_engine().await;
        add_second_workflow(&mut engine);

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("k1"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        let ack = engine
            .on_task_submit(
                "slack".into(),
                "review".into(),
                delivery("C1:100", Some("k1"), "one again"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Duplicate);
        let record = engine.db.get_task(id).unwrap().unwrap();
        assert_eq!(
            record.workflow, "implement",
            "a known key hands nothing over"
        );
        assert_eq!(record.state, TaskState::Failed, "and does not reopen");
    }

    /// At-least-once delivery: the same `message_key` twice must change
    /// nothing — not the ledger, not the state.
    #[tokio::test]
    async fn a_redelivered_message_is_a_duplicate_and_changes_nothing() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }

        let ack = engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Duplicate);
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done,
            "a re-delivery must not reopen the conversation"
        );
        assert_eq!(engine.db.list_task_messages(id).unwrap().len(), 1);
    }

    /// A message arriving mid-flight waits in the ledger; requeueing a running
    /// task would throw away the work in progress.
    #[tokio::test]
    async fn a_message_for_a_running_conversation_is_queued_without_touching_its_state() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Start, None).unwrap();

        let ack = engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:200"), "two"),
            )
            .unwrap();
        assert_eq!(ack.status, TaskSubmitStatus::Accepted);
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Running,
            "an in-flight conversation keeps running"
        );
        assert_eq!(engine.db.pending_task_messages(id).unwrap().len(), 2);
    }

    /// Sources that send one message per task (GitHub issues, Notion pages)
    /// omit `message_key`. They must behave exactly as before: ingested once,
    /// every re-delivery a `Duplicate`.
    #[tokio::test]
    async fn a_source_without_message_keys_keeps_its_at_most_once_behaviour() {
        let mut engine = ingest_test_engine().await;
        let issue = delivery("42", None, "fix the bug");

        assert_eq!(
            engine
                .on_task_submit("slack".into(), "implement".into(), issue.clone())
                .unwrap()
                .status,
            TaskSubmitStatus::Accepted
        );
        let id = engine.db.find_by_source("slack", "42").unwrap().unwrap().id;
        for event in [
            TaskEvent::Dispatch,
            TaskEvent::Start,
            TaskEvent::BeginPublish,
            TaskEvent::Complete,
        ] {
            engine.db.apply_event(id, event, None).unwrap();
        }

        assert_eq!(
            engine
                .on_task_submit("slack".into(), "implement".into(), issue)
                .unwrap()
                .status,
            TaskSubmitStatus::Duplicate,
            "without a message_key the conversation id is the dedup key"
        );
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done,
            "and a finished task is not reopened by a re-delivery"
        );
        assert_eq!(engine.db.list_task_messages(id).unwrap().len(), 1);
    }

    /// Even a brand-new conversation gets a ledger row, so the ledger is the
    /// whole history rather than "everything after the first message".
    #[tokio::test]
    async fn a_new_conversation_starts_its_ledger_with_the_first_message() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        let messages = engine.db.list_task_messages(id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_key, "C1:100");
        assert_eq!(messages[0].body, "one");
        assert!(messages[0].processed_at.is_none(), "queued for dispatch");
        // The payload keeps the whole normalized task for the audit trail.
        let payload: Task = serde_json::from_str(&messages[0].payload).unwrap();
        assert_eq!(payload.message_key.as_deref(), Some("C1:100"));
    }

    /// `task retry` after an agent failure must resend what the failed run was
    /// given; requeueing the task alone would dispatch an empty prompt.
    #[tokio::test]
    async fn retry_puts_the_failed_dispatch_s_messages_back_on_the_queue() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // Dispatched (messages handed over), then the agent failed.
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.mark_messages_processed(id).unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();
        assert!(engine.db.pending_task_messages(id).unwrap().is_empty());

        let (state, requeued) = engine.db.retry_task(id, None).unwrap();
        assert_eq!(state, TaskState::Queued);
        assert_eq!(requeued, 1);
        assert_eq!(
            conversation_prompt(&engine.db.pending_task_messages(id).unwrap()),
            Some("one".to_string()),
            "the retry has something to say again"
        );
    }

    /// Messages that arrived while the agent was working are collected once it
    /// is done — that is what makes ingest's "leave a running task alone"
    /// safe.
    #[tokio::test]
    async fn a_finished_conversation_with_unsent_messages_is_requeued() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.mark_messages_processed(id).unwrap();
        engine.db.apply_event(id, TaskEvent::Start, None).unwrap();

        // A message lands mid-flight: ingest leaves the running task alone.
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:200"), "two"),
            )
            .unwrap();
        engine
            .requeue_conversations_with_unsent_messages()
            .await
            .unwrap();
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Running,
            "a working conversation is never interrupted"
        );

        // Once it finishes, the sweep picks the message up.
        engine
            .db
            .apply_event(id, TaskEvent::BeginPublish, None)
            .unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::Complete, None)
            .unwrap();
        engine
            .requeue_conversations_with_unsent_messages()
            .await
            .unwrap();
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Queued
        );
        assert_eq!(
            conversation_prompt(&engine.db.pending_task_messages(id).unwrap()),
            Some("two".to_string()),
            "only the unsent message is resent"
        );

        // Idempotent: with nothing unsent, a finished conversation stays put.
        engine.db.mark_messages_processed(id).unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Start, None).unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::BeginPublish, None)
            .unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::Complete, None)
            .unwrap();
        engine
            .requeue_conversations_with_unsent_messages()
            .await
            .unwrap();
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Done
        );
    }

    /// A dispatch that fails leaves its messages unsent, so requeueing on
    /// failure would re-dispatch, fail identically, and go round again every
    /// tick — with a notification each time — for exactly the errors that need
    /// a person. `Failed` is therefore not swept.
    #[tokio::test]
    async fn a_failed_conversation_is_not_requeued_by_the_sweep() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // A dispatch failure: the message stays unsent (it is stamped only on
        // success) and the task lands in Failed.
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();
        assert_eq!(engine.db.pending_task_messages(id).unwrap().len(), 1);

        for _ in 0..3 {
            engine
                .requeue_conversations_with_unsent_messages()
                .await
                .unwrap();
        }
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Failed,
            "sweeping Failed would loop on any permanent dispatch error"
        );
        // `task retry` is the deliberate way back, and it brings the message.
        let (state, _) = engine.db.retry_task(id, None).unwrap();
        assert_eq!(state, TaskState::Queued);
        assert_eq!(engine.db.pending_task_messages(id).unwrap().len(), 1);
    }

    /// `task retry` on a run that failed *before* dispatch must not drag the
    /// previous, already-answered batch back in alongside the waiting one.
    #[tokio::test]
    async fn retry_does_not_revive_an_already_answered_batch() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        // First message dispatched and answered.
        engine
            .db
            .apply_event(id, TaskEvent::Dispatch, None)
            .unwrap();
        engine.db.mark_messages_processed(id).unwrap();
        engine.db.apply_event(id, TaskEvent::Start, None).unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::BeginPublish, None)
            .unwrap();
        engine
            .db
            .apply_event(id, TaskEvent::Complete, None)
            .unwrap();

        // A second message reopens it, but this dispatch fails before it is
        // handed over — so "two" is still unsent.
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:200"), "two"),
            )
            .unwrap();
        engine.db.apply_event(id, TaskEvent::Fail, None).unwrap();

        let (_, requeued) = engine.db.retry_task(id, None).unwrap();
        assert_eq!(
            requeued, 0,
            "nothing was handed over, so nothing to reclaim"
        );
        assert_eq!(
            conversation_prompt(&engine.db.pending_task_messages(id).unwrap()),
            Some("two".to_string()),
            "the answered message must not be replayed alongside the new one"
        );
    }

    /// `task/lookup` (#242) tells a source whether it is about to open a new
    /// conversation, so it can skip resolving a repository for a reply.
    #[tokio::test]
    async fn task_lookup_answers_what_the_orchestrator_knows() {
        let mut engine = ingest_test_engine().await;

        async fn ask(
            engine: &mut Engine<crate::adapters::git::SystemGitRunner, NoLlmRouter>,
            source: &str,
            task_id: &str,
        ) -> TaskLookupResult {
            let (tx, rx) = tokio::sync::oneshot::channel();
            engine
                .on_event(PluginEvent::TaskLookup {
                    source: source.to_string(),
                    task_id: task_id.to_string(),
                    respond: tx,
                })
                .await
                .unwrap();
            rx.await.unwrap().unwrap()
        }

        // Nothing ingested yet: a new conversation.
        assert_eq!(
            ask(&mut engine, "slack", "C1:100").await,
            TaskLookupResult {
                known: false,
                repo: None
            }
        );

        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;

        // Known, but repository selection has not settled: `repo: None` means
        // "no hint", not "no repository".
        assert_eq!(
            ask(&mut engine, "slack", "C1:100").await,
            TaskLookupResult {
                known: true,
                repo: None
            }
        );

        engine.db.set_repo(id, "totsuka").unwrap();
        assert_eq!(
            ask(&mut engine, "slack", "C1:100").await,
            TaskLookupResult {
                known: true,
                repo: Some("totsuka".to_string())
            }
        );

        // Conversations are scoped to their source: another plugin naming the
        // same id learns nothing.
        assert_eq!(
            ask(&mut engine, "github", "C1:100").await,
            TaskLookupResult {
                known: false,
                repo: None
            }
        );
    }

    /// The forwarder — not the engine — is where a plugin's claim about which
    /// source it is gets discarded. A plugin must not be able to read another
    /// source's conversations by naming it in the params, so pin the
    /// enforcement at the three lines that do it.
    #[tokio::test]
    async fn task_lookup_takes_its_source_from_the_connection_not_the_params() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (write_tx, _write_rx) = mpsc::unbounded_channel();
        let budgets = PluginRequestBudgets {
            submit: Arc::new(Semaphore::new(1)),
            lookup: Arc::new(Semaphore::new(1)),
        };

        forward_plugin_request(
            "slack",
            IncomingRequest {
                method: method::TASK_LOOKUP.to_string(),
                // The plugin claims to be `github`.
                params: Some(serde_json::json!({
                    "source": "github", "task_id": "C1:100",
                })),
                responder: crate::adapters::plugin_host::Responder::for_test(
                    plugin_protocol::RequestId::Str("lookup-0".into()),
                    write_tx,
                ),
            },
            &event_tx,
            &budgets,
        );

        let event = event_rx.recv().await.expect("an event was forwarded");
        let PluginEvent::TaskLookup {
            source, task_id, ..
        } = event
        else {
            panic!("expected a TaskLookup event");
        };
        assert_eq!(source, "slack", "the connection wins over the params");
        assert_eq!(task_id, "C1:100");
    }

    /// A plugin-initiated method the Orchestrator does not implement is still
    /// answered `METHOD_NOT_FOUND`, unchanged by the forwarder's split.
    #[tokio::test]
    async fn an_unknown_plugin_initiated_method_is_still_method_not_found() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (write_tx, mut write_rx) = mpsc::unbounded_channel();
        let budgets = PluginRequestBudgets {
            submit: Arc::new(Semaphore::new(1)),
            lookup: Arc::new(Semaphore::new(1)),
        };

        forward_plugin_request(
            "slack",
            IncomingRequest {
                method: "task/invent".to_string(),
                params: None,
                responder: crate::adapters::plugin_host::Responder::for_test(
                    plugin_protocol::RequestId::Str("x-1".into()),
                    write_tx,
                ),
            },
            &event_tx,
            &budgets,
        );

        let line = write_rx.recv().await.expect("an answer was written");
        let answer: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            answer["error"]["code"],
            plugin_protocol::error_code::METHOD_NOT_FOUND
        );
        assert!(
            event_rx.try_recv().is_err(),
            "an unknown method never reaches the engine loop"
        );
    }

    /// A human's `task cancel` outranks messages that were already waiting:
    /// only a message arriving *after* the cancel reopens it (through ingest).
    #[tokio::test]
    async fn a_cancelled_conversation_is_not_revived_by_leftover_messages() {
        let mut engine = ingest_test_engine().await;
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:100"), "one"),
            )
            .unwrap();
        let id = engine
            .db
            .find_by_source("slack", "C1:100")
            .unwrap()
            .unwrap()
            .id;
        engine.db.apply_event(id, TaskEvent::Cancel, None).unwrap();

        engine
            .requeue_conversations_with_unsent_messages()
            .await
            .unwrap();
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Cancelled
        );

        // ...but a message arriving now is a fresh instruction.
        engine
            .on_task_submit(
                "slack".into(),
                "implement".into(),
                delivery("C1:100", Some("C1:200"), "two"),
            )
            .unwrap();
        assert_eq!(
            engine.db.get_task(id).unwrap().unwrap().state,
            TaskState::Queued
        );
    }
}
