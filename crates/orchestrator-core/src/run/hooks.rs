//! Hook-signal handling for the run loop (#131/#138).
//!
//! Split out of [`run`](super):
//! this module owns everything that turns a normalized Claude Code hook signal
//! into a state transition. [`Engine::on_signal`] is the entry point — an
//! idempotent record → task resolution → state transition → verification →
//! output pipeline — plus the escalation, timeout-sweep, and spool-recovery
//! machinery, and the `Engine::hook_launch` dispatch helper.
//!
//! The existing terminal machinery (`apply_agent_state`,
//! `finalize_success`, the output policy) is reused
//! **unchanged**: a hook `Stop{Completed}` for an `llm`/`none` workflow is just
//! an `AgentState::Done` from a different source, so it flows through the same
//! publish path. The hook-specific parts are verification (`human` →
//! `Verifying`), escalation (UNKNOWN streak / timeout), and using the marker-
//! stripped `last_assistant_message` as the publish artifact (R-07/R-11).

use std::collections::BTreeMap;
use std::path::PathBuf;

use plugin_protocol::method;
use plugin_protocol::methods::{
    AgentState, DiagnosticsSnapshotParams, DiagnosticsSnapshotResult, NotifierEvent,
    SessionFocusParams, SessionFocusResult,
};

use super::{Engine, EngineError, notify_all, workflows_by_name};
use crate::adapters::hook_uds;
use crate::adapters::state_db::{HookEventInsert, HookEventOutcome, StateError, TaskRecord};
use crate::config::{DEFAULT_BLOCK_RETRY_LIMIT, DEFAULT_WORKFLOW_TIMEOUT_SECS, VerificationMode};
use crate::domain::signal::{AgentSignal, SignalEvent, StopStatus};
use crate::domain::state::{TaskEvent, TaskState};
use crate::ports::git::GitRunner;
use crate::ports::llm::LlmRouter;
use crate::ports::signal_ingress::FocusOutcome;

impl<G: GitRunner, L: LlmRouter> Engine<G, L> {
    /// Interpret one normalized hook signal (#138): resolve its task, record it
    /// idempotently, then drive the state machine per the signal's event.
    ///
    /// Delivered from the UDS receiver via
    /// `PluginEvent::HookSignal` and from
    /// [`replay_spool`](Self::replay_spool). Public so integration tests can
    /// feed signals directly.
    pub async fn on_signal(&mut self, sig: AgentSignal) -> Result<(), EngineError> {
        // Resolve job_id → task (E-09: never guessed from a session id). An
        // unknown task is accepted at the socket but parked here: no state
        // change, and the `hook_events` FK forbids logging against a task that
        // does not exist. A signal that cannot be correlated is left only in the
        // warn log and NEVER persisted to `hook_events` (its `task_id` is a
        // NOT NULL FK, #134) — this is intentional, so a malformed/stale job_id
        // can never corrupt state.
        let task_id = sig.job_id.task_id;
        let Some(record) = self.db.get_task(task_id)? else {
            tracing::warn!(job_id = %sig.job_id, "hook signal for unknown task → warn-logged only, not persisted (E-09)");
            return Ok(());
        };

        // The task exists, so this signal proves liveness: bump the R-10 timeout
        // anchor FIRST, before the dedup short-circuit. A duplicate delivery must
        // still refresh the anchor — mid-turn Stops all collapse to a single
        // `heartbeat` idempotency key and re-send the same prompt_id, so gating
        // the touch behind `New` would let `sweep_signal_timeouts` falsely
        // escalate a task that is very much alive.
        self.db.touch_last_signal(task_id)?;

        // Idempotent record (D-05): a repeated delivery (multi-fire, spool
        // re-send, curl retry) is dropped before any state change.
        let (event_str, status_str) = event_and_status_strings(&sig.event);
        let insert = HookEventInsert {
            job_id: sig.job_id.to_string(),
            task_id,
            tool_session_id: sig.tool_session_id.clone(),
            prompt_id: sig.prompt_id.clone(),
            event: event_str.to_string(),
            status: status_str.map(str::to_string),
            payload: serde_json::to_string(&sig.payload).unwrap_or_else(|_| "{}".to_string()),
        };
        if self.db.record_hook_event(&insert)? == HookEventOutcome::Duplicate {
            tracing::debug!(job_id = %sig.job_id, event = event_str, "duplicate hook signal dropped after refreshing liveness (D-05)");
            return Ok(());
        }

        // The owning agent plugin (for slot resume / diagnostics). Empty when no
        // session was recorded yet — only used where a plugin is truly needed.
        let agent_plugin = self
            .db
            .latest_session(task_id)?
            .map(|s| s.plugin)
            .unwrap_or_default();

        match sig.event {
            SignalEvent::Stop {
                status,
                reason,
                last_assistant_message,
                transcript_path,
            } => {
                // Every Stop, whatever it says: this is the last moment before
                // the task can be published, cleaned up, or retried, and all
                // three read the recorded branch. A `NeedsInput` stop matters
                // most — it is followed by a human reply and a re-dispatch,
                // which is exactly when a stale record would send the task
                // back through worktree creation.
                self.sync_branch(task_id)?;
                match status {
                    StopStatus::Completed => {
                        self.on_stop_completed(
                            &record,
                            &agent_plugin,
                            last_assistant_message,
                            transcript_path,
                        )
                        .await?
                    }
                    StopStatus::NeedsInput => {
                        self.on_stop_needs_input(&record, &agent_plugin, reason)
                            .await?
                    }
                    StopStatus::Failed => self.on_stop_failed(&record, reason).await?,
                    StopStatus::Unknown => self.on_stop_unknown(&record).await?,
                }
            }
            // A permission / idle prompt: surface it to the human but keep the
            // task running and holding its slot. Approval-waiting is distinct
            // from question-waiting (R-08) — only `Stop{NeedsInput}` moves the
            // task to `WaitingInput`.
            SignalEvent::Notification { message } => {
                notify_all(
                    &self.plugins.notifiers,
                    NotifierEvent::WaitingInput,
                    &record,
                    message,
                );
            }
            SignalEvent::SessionStart { tool_session_id } => {
                self.on_session_start(&record, sig.job_id.session_row, &tool_session_id)?;
            }
            SignalEvent::SessionEnd { reason } => self.on_session_end(&record, reason),
            // Liveness only: the anchor was already bumped above.
            SignalEvent::Heartbeat => {}
        }
        Ok(())
    }

    /// `Stop{Completed}`: publish directly (`llm`/`none`) or await human
    /// verification (`human`, D-01). Reuses the existing Done/publish path.
    async fn on_stop_completed(
        &mut self,
        record: &TaskRecord,
        agent_plugin: &str,
        last_assistant_message: Option<String>,
        _transcript_path: Option<String>,
    ) -> Result<(), EngineError> {
        // Only a task still in the agent pipeline can complete; a stray
        // completion for an already-finished/verifying/publishing task is
        // ignored so it can never double-finalize.
        if !matches!(
            record.state,
            TaskState::Dispatched
                | TaskState::Running
                | TaskState::WaitingInput
                | TaskState::Escalated
        ) {
            tracing::debug!(task_id = record.id, state = %record.state, "ignoring COMPLETED in a non-pipeline state");
            return Ok(());
        }

        // R-07/R-11: the completion artifact is the last assistant message with
        // the status marker stripped. Stash it so the existing publish path
        // (BeginPublish → finalize_success) uses it as the output.
        if let Some(msg) = &last_assistant_message {
            self.agent_output
                .insert(record.id, strip_status_markers(msg));
        }

        match self.verification_for(record) {
            VerificationMode::Llm | VerificationMode::None => {
                // `Escalated` is outside `apply_agent_state`'s supported set, so
                // drive that one transition explicitly; every other
                // pre-completion state reuses the Done path unchanged.
                if record.state == TaskState::Escalated {
                    self.db.apply_event(
                        record.id,
                        TaskEvent::BeginPublish,
                        Some(serde_json::json!({
                            "kind": "hook_complete",
                            "publish_artifact": self.agent_output.get(&record.id),
                        })),
                    )?;
                    self.finalize_success(record).await?;
                } else {
                    self.apply_agent_state(record.id, agent_plugin, AgentState::Done, None)
                        .await?;
                }
            }
            VerificationMode::Human => {
                // Move from Dispatched into the pipeline first if the completion
                // is the very first signal, then self-report → Verifying.
                if record.state == TaskState::Dispatched {
                    self.db.apply_event(
                        record.id,
                        TaskEvent::Start,
                        Some(serde_json::json!({ "kind": "hook_start" })),
                    )?;
                }
                // Persist the artifact on the transition so a restart can verify
                // and publish without re-deriving it (#133 recovery safety).
                self.db.apply_event(
                    record.id,
                    TaskEvent::SelfReportComplete,
                    Some(serde_json::json!({
                        "kind": "self_report",
                        "publish_artifact": self.agent_output.get(&record.id),
                    })),
                )?;
                notify_all(
                    &self.plugins.notifiers,
                    NotifierEvent::VerificationPending,
                    record,
                    Some("completion self-reported → `totsuka task verify`".to_string()),
                );
                tracing::info!(task_id = record.id, "awaiting human verification (D-01)");
            }
        }
        Ok(())
    }

    /// `Stop{NeedsInput}`: park the task in `WaitingInput` (D-07).
    async fn on_stop_needs_input(
        &mut self,
        record: &TaskRecord,
        agent_plugin: &str,
        reason: Option<String>,
    ) -> Result<(), EngineError> {
        match record.state {
            // Already waiting: idempotent no-op.
            TaskState::WaitingInput => {}
            // Resume from an escalation straight into WaitingInput.
            TaskState::Escalated => {
                self.db.apply_event(
                    record.id,
                    TaskEvent::WaitInput,
                    Some(serde_json::json!({ "kind": "hook", "reason": reason })),
                )?;
                self.release_slot(record.id);
                notify_all(
                    &self.plugins.notifiers,
                    NotifierEvent::WaitingInput,
                    record,
                    reason,
                );
            }
            TaskState::Dispatched | TaskState::Running => {
                self.apply_agent_state(record.id, agent_plugin, AgentState::WaitingInput, reason)
                    .await?;
            }
            _ => {
                tracing::debug!(task_id = record.id, state = %record.state, "ignoring NEEDS_INPUT in a non-pipeline state")
            }
        }
        Ok(())
    }

    /// `Stop{Failed}`: fail the task, keeping the marker's reason. (Done here
    /// rather than via `apply_agent_state`, which hardcodes the failure detail.)
    async fn on_stop_failed(
        &mut self,
        record: &TaskRecord,
        reason: Option<String>,
    ) -> Result<(), EngineError> {
        if record.state.is_terminal() {
            return Ok(());
        }
        self.db.apply_event(
            record.id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "kind": "hook", "reason": reason })),
        )?;
        self.release_slot(record.id);
        self.drop_task_sessions(record.id);
        self.agent_output.remove(&record.id);
        self.stats.failed += 1;
        self.write_back_status(record, false).await;
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Failed,
            record,
            reason,
        );
        tracing::warn!(task_id = record.id, "task failed (hook FAILED)");
        Ok(())
    }

    /// `Stop{Unknown}`: no transition; escalate once the consecutive-UNKNOWN
    /// streak reaches the block-retry limit (D-02, recomputed from the log).
    async fn on_stop_unknown(&mut self, record: &TaskRecord) -> Result<(), EngineError> {
        let streak = self.db.unknown_stop_streak(record.id)?;
        let limit = self.block_retry_limit();
        if streak >= limit {
            self.escalate(
                record,
                format!("{streak} consecutive UNKNOWN stops (limit {limit})"),
            )
            .await?;
        } else {
            tracing::info!(
                task_id = record.id,
                streak,
                "UNKNOWN stop recorded; below escalation threshold"
            );
        }
        Ok(())
    }

    /// `SessionStart`: establish the Claude session-id correlation (E-09). A
    /// mismatch against a previously-recorded id is a correlation anomaly —
    /// warn only; it never escalates (a fresh `--resume` legitimately changes
    /// the id).
    fn on_session_start(
        &self,
        record: &TaskRecord,
        session_row: i64,
        tool_session_id: &str,
    ) -> Result<(), EngineError> {
        if tool_session_id.is_empty() {
            return Ok(());
        }
        let prior = self
            .db
            .list_sessions(record.id)?
            .into_iter()
            .find(|s| s.id == session_row)
            .and_then(|s| s.tool_session_id);
        if let Some(existing) = &prior
            && existing != tool_session_id
        {
            tracing::warn!(
                task_id = record.id,
                "SessionStart reported tool session id differs from the recorded one (correlation anomaly, E-09); keeping the newest"
            );
        }
        match self.db.set_tool_session_id(session_row, tool_session_id) {
            Ok(()) => {}
            // The job_id's session_row does not exist: a stale/anomalous
            // correlation. Record nothing, do not fail.
            Err(StateError::NotFound(_)) => tracing::warn!(
                task_id = record.id,
                session_row,
                "SessionStart for an unknown session row → ignored (E-09)"
            ),
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// `SessionEnd`: record + warn, but never `Fail`. The `pane.exited` deadman
    /// and the timeout sweep own liveness; failing here would double-judge
    /// (D-10). The event is already persisted in `hook_events`.
    fn on_session_end(&self, record: &TaskRecord, reason: Option<String>) {
        if record.state.is_terminal() {
            return;
        }
        tracing::warn!(
            task_id = record.id,
            reason = ?reason,
            "agent session ended before completion → deferring to the timeout sweep / pane.exited deadman"
        );
    }

    /// Escalate a task to a human (D-02/D-03): capture a pane snapshot for the
    /// audit detail (R-10) if the plugin supports it, transition to `Escalated`
    /// (freeing its slot), and notify. `Escalated` is non-terminal: the next
    /// signal resumes the task.
    async fn escalate(&mut self, record: &TaskRecord, reason: String) -> Result<(), EngineError> {
        if record.state.is_terminal() || record.state == TaskState::Escalated {
            return Ok(());
        }
        let snapshot = self.diagnostics_snapshot(record).await;
        self.db.apply_event(
            record.id,
            TaskEvent::Escalate,
            Some(serde_json::json!({
                "kind": "escalate",
                "reason": reason,
                "diagnostics": snapshot,
            })),
        )?;
        self.release_slot(record.id);
        notify_all(
            &self.plugins.notifiers,
            NotifierEvent::Escalated,
            record,
            Some(reason),
        );
        tracing::warn!(task_id = record.id, "task escalated to a human (D-02/D-03)");
        Ok(())
    }

    /// Bring a task's pane to the foreground (F-94, `POST /focus` → here).
    ///
    /// Not a hook signal, but it shares this module's task→session→plugin
    /// resolution (see `diagnostics_snapshot`):
    /// resolve the task's latest session, gate on the agent's `pane_control`
    /// capability, and delegate `session/focus` — the session id stays opaque
    /// (F-37). Every "cannot focus" is a normal [`FocusOutcome`] with a
    /// reason, never an error: clicking a notification for a finished task,
    /// or one whose agent cannot focus panes, must degrade quietly.
    pub async fn focus_task(&self, task_id: i64) -> FocusOutcome {
        let record = match self.db.get_task(task_id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return FocusOutcome::not(format!(
                    "task {task_id} not found → `totsuka task list` shows known ids"
                ));
            }
            Err(e) => return FocusOutcome::not(format!("state DB error: {e}")),
        };
        let session = match self.db.latest_session(record.id) {
            Ok(Some(session)) => session,
            Ok(None) => {
                return FocusOutcome::not(format!(
                    "task {task_id} has no agent session (never dispatched)"
                ));
            }
            Err(e) => return FocusOutcome::not(format!("state DB error: {e}")),
        };
        let Some(agent) = self.plugins.agents.get(&session.plugin) else {
            return FocusOutcome::not(format!(
                "agent plugin `{}` is not running in this orchestrator",
                session.plugin
            ));
        };
        if !agent.capabilities().pane_control {
            return FocusOutcome::not(format!(
                "agent plugin `{}` does not support pane focus (no `pane_control` capability)",
                session.plugin
            ));
        }
        let params = SessionFocusParams {
            session_id: session.session_id.clone(),
        };
        match agent
            .call::<_, SessionFocusResult>(method::SESSION_FOCUS, &params)
            .await
        {
            Ok(result) if result.focused => FocusOutcome::focused(),
            Ok(_) => FocusOutcome::not("the pane is already closed"),
            Err(e) => FocusOutcome::not(format!("session/focus failed: {e}")),
        }
    }

    /// Capture a pane snapshot for escalation diagnostics (R-10), if the task's
    /// agent plugin declares the `diagnostics_snapshot` capability. Best effort:
    /// any failure yields `None`.
    async fn diagnostics_snapshot(&self, record: &TaskRecord) -> Option<String> {
        let session = self.db.latest_session(record.id).ok().flatten()?;
        let agent = self.plugins.agents.get(&session.plugin)?;
        if !agent.capabilities().diagnostics_snapshot {
            return None;
        }
        let params = DiagnosticsSnapshotParams {
            session_id: session.session_id.clone(),
        };
        match agent
            .call::<_, DiagnosticsSnapshotResult>(method::DIAGNOSTICS_SNAPSHOT, &params)
            .await
        {
            Ok(result) => result.text,
            Err(e) => {
                tracing::warn!(task_id = record.id, "diagnostics/snapshot failed: {e}");
                None
            }
        }
    }

    /// Escalate hook-dispatched tasks that have gone silent past their workflow
    /// timeout (D-03). Runs each `cycle()`. Only actively-executing states are
    /// swept — `WaitingInput`/`Verifying`/`Escalated` are intentionally paused
    /// on a human, not silent agents — and only tasks that have received at
    /// least one signal (a set `last_signal_at` is the anchor).
    pub async fn sweep_signal_timeouts(&mut self) -> Result<(), EngineError> {
        let now = self.clock.now_utc();
        let mut timed_out: Vec<TaskRecord> = Vec::new();
        for state in [
            TaskState::Dispatched,
            TaskState::Running,
            TaskState::Publishing,
        ] {
            for record in self.db.tasks_in_state(state)? {
                let Some(last) = record.last_signal_at.as_deref() else {
                    continue;
                };
                let Ok(last_at) = time::OffsetDateTime::parse(
                    last,
                    &time::format_description::well_known::Rfc3339,
                ) else {
                    continue;
                };
                let timeout = self.workflow_timeout_secs(&record.workflow);
                if (now - last_at).whole_seconds() > timeout as i64 {
                    timed_out.push(record);
                }
            }
        }
        for record in timed_out {
            let secs = self.workflow_timeout_secs(&record.workflow);
            self.escalate(
                &record,
                format!("no hook signal for over {secs}s (timeout)"),
            )
            .await?;
        }
        Ok(())
    }

    /// Recover hook signals that a failed POST spooled to `[hooks].spool_dir` as
    /// NDJSON (E-07). Each line is normalized through the *same* parser a live
    /// POST uses and fed to [`on_signal`](Self::on_signal); the file is deleted
    /// after. The idempotency key (D-05) makes read-all-then-delete safe, even
    /// if a line was already delivered live.
    pub async fn replay_spool(&mut self) -> Result<(), EngineError> {
        let Some(dir) = self
            .settings
            .hook
            .as_ref()
            .and_then(|h| h.spool_dir.clone())
        else {
            return Ok(());
        };
        let read = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), "spool read failed: {e}");
                return Ok(());
            }
        };
        // Time-prefixed filenames replay oldest-first with a plain sort.
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .collect();
        files.sort();
        for path in files {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(file = %path.display(), "spool file read failed: {e}");
                    continue;
                }
            };
            let mut had_parse_error = false;
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match hook_uds::parse_signal(line.as_bytes()) {
                    Ok(sig) => self.on_signal(sig).await?,
                    Err(e) => {
                        had_parse_error = true;
                        tracing::warn!(file = %path.display(), "unparseable spool line: {e}");
                    }
                }
            }
            if had_parse_error {
                // A partial write or a corrupt line must not cost the whole
                // file: quarantine it (rename to `<name>.corrupt`, no longer a
                // `*.jsonl`) for inspection instead of deleting. Only fully
                // clean files are removed.
                let quarantine = PathBuf::from(format!("{}.corrupt", path.display()));
                if let Err(e) = std::fs::rename(&path, &quarantine) {
                    tracing::warn!(file = %path.display(), "spool quarantine rename failed: {e} → left in place");
                } else {
                    tracing::warn!(
                        file = %path.display(),
                        quarantine = %quarantine.display(),
                        "spool file had unparseable lines → quarantined instead of deleted"
                    );
                }
            } else if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(file = %path.display(), "spool file delete failed: {e}");
            }
        }
        Ok(())
    }

    /// Base `HookLaunchSpec` material for a workflow: the rendered `--settings`
    /// path plus the endpoint/token/spool-dir env (the per-dispatch
    /// `TOTSUKA_JOB_ID` is added by the caller). `None` when this run has no
    /// hook runtime or the workflow has no rendered settings file — the dispatch
    /// then falls back to the non-hook path.
    pub(crate) fn hook_launch(&self, workflow: &str) -> Option<(String, BTreeMap<String, String>)> {
        let hook = self.settings.hook.as_ref()?;
        let settings_path = hook.settings_paths.get(workflow)?;
        let mut env = BTreeMap::new();
        env.insert(
            "TOTSUKA_HOOK_ENDPOINT".to_string(),
            hook.socket_path.display().to_string(),
        );
        if let Some(token) = &hook.auth_token {
            env.insert("TOTSUKA_HOOK_TOKEN".to_string(), token.expose().to_string());
        }
        if let Some(spool) = &hook.spool_dir {
            env.insert(
                "TOTSUKA_HOOK_SPOOL_DIR".to_string(),
                spool.display().to_string(),
            );
        }
        Some((settings_path.display().to_string(), env))
    }

    /// The **effective** verification mode for a task: its workflow's
    /// configured mode (default `llm` if the workflow vanished from config —
    /// the publish path then fails safe via `finalize_success`), degraded to
    /// `human` when the task's AI tool cannot run llm verification.
    ///
    /// `llm` verification is an in-session prompt-type `Stop` hook running the
    /// rubric ([ADR-0004](https://github.com/tomoya-k31/totsuka/blob/main/docs/decisions/adr-0004-hook-completion-signal.md)
    /// decision 2), and only Claude-kind tools have prompt-type hooks —
    /// `hooks::render_settings` emits the rubric hook into Claude's
    /// `--settings` file, while Codex registers command-type hooks only and
    /// OpenCode runs a JS plugin. Without this degradation the `Llm` arm of
    /// [`on_stop_completed`](Self::on_stop_completed) shares its branch with
    /// `None` and publishes straight away, so a non-claude tool silently
    /// produced *unverified* output while the config still claimed `llm`
    /// (#301). `ToolCapabilities::prompt_verification` declared the constraint
    /// but nothing read it.
    ///
    /// The tool is re-resolved here rather than persisted at dispatch: the
    /// inputs (`[[workflows]].tool` > `[[repositories]].tool` > `default_tool`)
    /// all live in `EngineSettings`, which is built once at startup and never
    /// mutated, so within one run this resolves exactly as `dispatch_one` did.
    /// Across a restart with an edited config, the *current* config is the
    /// right answer anyway.
    fn verification_for(&self, record: &TaskRecord) -> VerificationMode {
        let workflows = workflows_by_name(&self.settings.workflows);
        let Some(wf) = workflows.get(record.workflow.as_str()).copied() else {
            return VerificationMode::default();
        };
        if wf.verification != VerificationMode::Llm {
            return wf.verification;
        }

        let repo_tool = record.repo.as_deref().and_then(|name| {
            self.settings
                .repos
                .iter()
                .find(|r| r.name == name)
                .and_then(|r| r.tool.as_deref())
        });
        let tool_name = crate::tool::resolve_tool_name(
            wf.tool.as_deref(),
            repo_tool,
            &self.settings.default_tool,
        );
        // An unknown tool name cannot happen for a dispatched task
        // (`dispatch_one` fails the dispatch first), so leave the configured
        // mode alone rather than degrading on a lookup miss.
        match self.settings.tools.get(&tool_name) {
            Some(tool) if !tool.capabilities().prompt_verification => {
                tracing::warn!(
                    task_id = record.id,
                    workflow = %record.workflow,
                    tool = %tool_name,
                    kind = tool.kind.as_str(),
                    "verification = llm needs a prompt-type Stop hook, which this tool does not have → falling back to human verification (#301); pin a claude-kind tool or set verification = \"human\""
                );
                VerificationMode::Human
            }
            _ => VerificationMode::Llm,
        }
    }

    /// The configured UNKNOWN-stop escalation threshold (D-02).
    fn block_retry_limit(&self) -> u32 {
        self.settings
            .hook
            .as_ref()
            .map(|h| h.block_retry_limit)
            .unwrap_or(DEFAULT_BLOCK_RETRY_LIMIT)
    }

    /// A workflow's silence timeout in seconds (D-03).
    fn workflow_timeout_secs(&self, workflow: &str) -> u64 {
        workflows_by_name(&self.settings.workflows)
            .get(workflow)
            .and_then(|w| w.timeout_secs)
            .unwrap_or(DEFAULT_WORKFLOW_TIMEOUT_SECS)
    }
}

/// The `hook_events` `(event, status)` strings for a signal (D-05 / N-01). The
/// event column is the lowercase kind; `status` is the uppercase marker for a
/// `Stop`, matching [`StateDb::unknown_stop_streak`](crate::adapters::StateDb).
fn event_and_status_strings(event: &SignalEvent) -> (&'static str, Option<&'static str>) {
    match event {
        SignalEvent::Stop { status, .. } => (
            "stop",
            Some(match status {
                StopStatus::Completed => "COMPLETED",
                StopStatus::NeedsInput => "NEEDS_INPUT",
                StopStatus::Failed => "FAILED",
                StopStatus::Unknown => "UNKNOWN",
            }),
        ),
        SignalEvent::Notification { .. } => ("notification", None),
        SignalEvent::SessionStart { .. } => ("session_start", None),
        SignalEvent::SessionEnd { .. } => ("session_end", None),
        SignalEvent::Heartbeat => ("heartbeat", None),
    }
}

/// Remove every status-marker span from an assistant message, leaving the
/// human-facing prose to publish (R-07/R-11). Markers may be inline (the hook
/// greps them anywhere), so spans — not whole lines — are stripped. Mirrors
/// `on-stop.sh`'s tolerance (#152, real-machine finding): agents routinely
/// normalise the doubled angle brackets, so `<STATUS:...>` with one bracket on
/// either side is a marker too — anything on-stop.sh reads as a marker must
/// never leak into the published reply.
fn strip_status_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("STATUS:") {
        // Consume up to 2 `<` immediately before the keyword; no `<` means this
        // is prose mentioning STATUS:, not a marker.
        let mut start = pos;
        for _ in 0..2 {
            if rest[..start].ends_with('<') {
                start -= 1;
            }
        }
        if start == pos {
            out.push_str(&rest[..pos + "STATUS:".len()]);
            rest = &rest[pos + "STATUS:".len()..];
            continue;
        }
        out.push_str(&rest[..start]);
        match rest[pos..].find('>') {
            Some(gt) => {
                let mut end = pos + gt + 1;
                if rest[end..].starts_with('>') {
                    end += 1;
                }
                rest = &rest[end..];
            }
            // Unterminated marker fragment: drop the remainder.
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_status_markers_removes_inline_and_standalone() {
        assert_eq!(strip_status_markers("done <<STATUS:COMPLETED>>"), "done");
        assert_eq!(
            strip_status_markers("line one\n<<STATUS:COMPLETED>>\nline two"),
            "line one\n\nline two"
        );
        // Multiple markers, one with a reason attribute.
        assert_eq!(
            strip_status_markers("a <<STATUS:NEEDS_INPUT reason=\"x\">> b <<STATUS:COMPLETED>>"),
            "a  b"
        );
        // No marker → unchanged (trimmed).
        assert_eq!(strip_status_markers("  plain answer  "), "plain answer");
        // Unterminated fragment → dropped.
        assert_eq!(strip_status_markers("keep <<STATUS:oops"), "keep");
    }

    #[test]
    fn strip_status_markers_removes_single_bracket_markers_too() {
        // Real agents normalise the doubled brackets (#152); whatever on-stop.sh
        // reads as a marker must not leak into the published reply.
        assert_eq!(strip_status_markers("done\n<STATUS:COMPLETED>"), "done");
        assert_eq!(strip_status_markers("done <<STATUS:COMPLETED>"), "done");
        assert_eq!(
            strip_status_markers("wait <STATUS:NEEDS_INPUT reason=\"branch?\">"),
            "wait"
        );
        // Prose mentioning STATUS: without brackets is NOT a marker.
        assert_eq!(
            strip_status_markers("the STATUS: field is unrelated"),
            "the STATUS: field is unrelated"
        );
    }

    #[test]
    fn event_and_status_strings_match_the_db_vocabulary() {
        let stop = SignalEvent::Stop {
            status: StopStatus::Unknown,
            reason: None,
            last_assistant_message: None,
            transcript_path: None,
        };
        assert_eq!(event_and_status_strings(&stop), ("stop", Some("UNKNOWN")));
        assert_eq!(
            event_and_status_strings(&SignalEvent::Heartbeat),
            ("heartbeat", None)
        );
        assert_eq!(
            event_and_status_strings(&SignalEvent::SessionEnd { reason: None }),
            ("session_end", None)
        );
    }
}
