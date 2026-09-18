//! The orca adapter logic (F-30〜F-38): translate the Orchestrator's agent_ide
//! calls into `orca` CLI invocations.
//!
//! It keeps the **herdr plugin's contract** — hook-reported completion, an
//! exit deadman for a state stream, pane control and screen snapshots — and
//! absorbs the differences of orca's surface here. The facts it is written
//! against were measured on orca 1.4.205 (see
//! `ai-docs/references/orca-cli-control.md`):
//!
//! - **The session is an orca terminal**, identified by the handle
//!   `terminal create` returns (`term_<uuid>`). Handles are never reused — a
//!   closed terminal's handle answers `terminal_handle_stale`, an exited one
//!   keeps its record with `connected: false` — so unlike herdr's
//!   position-based pane ids there is no reuse to guard against. The
//!   `session_id` is the bare handle.
//! - **The worktree is totsuka's.** orca discovers every git worktree of a
//!   repository registered with it, including the one the Orchestrator cut for
//!   the task, so the terminal is opened *in* it by `path:` selector. The
//!   plugin used to run `worktree create` instead, which made a second
//!   worktree the Orchestrator knew nothing about, and `task/cancel` then
//!   deleted it with `worktree rm --force`.
//! - **The launch is `tool_launch`, verbatim** (#196): typed into the
//!   terminal's shell as `exec env K=V… program args…` (see [`crate::launch`]).
//!   With the Orchestrator's hook settings and env on it, Claude Code reports
//!   completion through its hooks, exactly as under herdr — which is why this
//!   plugin can declare `hook_completion`.
//! - **The prompt goes in through `terminal send --wait-submit`**, after
//!   `terminal wait --for tui-idle` says the TUI is up. For a Claude terminal
//!   orca observes the submission itself (`stages: [input_accepted,
//!   turn_started]`), and a multi-line prompt lands as one turn.
//! - **The prompt waits for orca to recognise the agent**, not just for
//!   `tui-idle`: sent any earlier it goes out as raw keystrokes and is lost.
//! - **The tab title is the ownership marker** (`totsuka {task_id}`, the same
//!   string herdr puts on its workspace label, which `doctor` strips to find
//!   the task), set with `terminal rename` once the prompt is in — the only
//!   title orca keeps against the agent's own.

use std::path::Path;
use std::time::Duration;

use plugin_protocol::methods::{
    AgentState, DiagnosticsSnapshotResult, NotReleased, SessionAttachResult, SessionFocusResult,
    SessionInfo, SessionListResult, SessionReleaseParams, SessionReleaseResult, StateNotification,
    TaskDispatchParams, TaskDispatchResult,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::cli::OrcaCli;
use crate::config::OrcaConfig;
use crate::error::OrcaError;
use crate::launch::shell_command;
use crate::state::map_orca_state;

/// The marker that says an orca terminal belongs to totsuka, followed by the
/// task's `source_task_id`. Set as the tab title (see `mark_owned`).
const OWNED_TITLE_PREFIX: &str = "totsuka ";

/// How long a freshly launched agent is given to reach `tui-idle` before the
/// prompt is sent anyway, in milliseconds.
///
/// Measured: a plain `claude` got there in ~4s. The wait is an optimisation
/// rather than a gate — a tool that never reports `tui-idle` (anything orca
/// has no state-dot integration for) still gets its prompt, only later.
const STARTUP_WAIT_MS: u64 = 60_000;

/// How long, after `tui-idle`, orca is given to recognise the agent in the
/// terminal before the prompt is sent anyway (see `wait_for_agent`).
const AGENT_DETECT_WAIT: Duration = Duration::from_secs(30);

/// The spacing of those checks.
const AGENT_DETECT_POLL: Duration = Duration::from_millis(500);

/// How long `terminal send --wait-submit` watches for the prompt to start a
/// turn, in seconds. Only an observation: orca never resends, so running out
/// of it costs a warning, not a duplicate prompt.
const SUBMIT_WAIT_SECS: u64 = 60;

/// One `terminal wait --for exit` in the deadman loop, in milliseconds. The
/// loop simply asks again on `timeout`, so this only bounds how long one CLI
/// process lives.
const EXIT_WAIT_MS: u64 = 600_000;

/// The deadman gives up — reporting `failed` — after this many consecutive
/// waits that failed for a reason other than orca's own `timeout`.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

/// The pause between two such failed waits.
const ERROR_BACKOFF: Duration = Duration::from_secs(2);

/// How many lines the stream-read fallback of a snapshot asks for.
const SNAPSHOT_LINES: u32 = 200;

/// How many rows a listing asks orca for. orca caps a page and flags
/// `truncated`; this is far past the number of terminals a machine runs.
const LIST_LIMIT: u32 = 500;

/// The longest worktree display name the identity report sets, in characters.
const DISPLAY_NAME_CHARS: usize = 80;

/// The orca agent_ide adapter, generic over its [`OrcaCli`].
pub struct OrcaAgent<C> {
    cli: C,
    config: OrcaConfig,
}

impl<C: OrcaCli> OrcaAgent<C> {
    /// A new adapter over `cli` using `config`.
    pub fn new(cli: C, config: OrcaConfig) -> Self {
        Self { cli, config }
    }

    /// Dispatch a task (F-31/F-37): open a terminal in the task's worktree
    /// running the resolved `tool_launch`, wait for the agent's TUI, submit the
    /// task prompt, and return the terminal handle as the session id.
    pub async fn dispatch(
        &self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, OrcaError> {
        let tool = params
            .tool_launch
            .as_ref()
            .ok_or(OrcaError::MissingToolLaunch)?;
        let command = shell_command(&tool.program, &tool.args, &tool.env);
        let created = self
            .cli
            .run(args([
                "terminal",
                "create",
                "--worktree",
                &format!("path:{}", params.worktree_path),
                // Only the initial title (see `mark_owned`), but it keeps the
                // tab recognisable for the seconds before the agent draws.
                "--title",
                &owned_title(&params),
                "--command",
                &command,
                "--json",
            ]))
            .await
            .map_err(|e| {
                if e.is_missing() {
                    OrcaError::WorktreeUnknown {
                        path: params.worktree_path.clone(),
                    }
                } else {
                    e
                }
            })?;
        let Some(handle) = created
            .get("terminal")
            .and_then(|t| t.get("handle"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            // The terminal may exist all the same, with an agent starting in
            // it, and a failed dispatch leaves the Orchestrator no id to
            // cancel it with. Find it by the title it was created with.
            self.abandon_untracked(&params).await;
            return Err(OrcaError::InvalidResponse(
                "`terminal create` returned no terminal handle".into(),
            ));
        };

        // From here on the terminal exists, so every failure has to take it
        // back down: a failed dispatch reports no session id, which leaves the
        // Orchestrator nothing to cancel with — and the agent would run on.
        self.report_identity(&params).await;
        self.apply_layout(&handle).await;
        if let Err(e) = self.start(&params, &handle).await {
            self.abandon(&handle).await;
            return Err(e);
        }
        self.mark_owned(&params, &handle).await;
        Ok(TaskDispatchResult { session_id: handle })
    }

    /// Wait for the agent's TUI, then submit the prompt.
    async fn start(&self, params: &TaskDispatchParams, handle: &str) -> Result<(), OrcaError> {
        match self
            .cli
            .run(args([
                "terminal",
                "wait",
                "--terminal",
                handle,
                "--for",
                "tui-idle",
                "--timeout-ms",
                &STARTUP_WAIT_MS.to_string(),
                "--json",
            ]))
            .await
        {
            // A wait can be satisfied by the process exiting: the CLI died
            // during startup, and there is nothing to prompt.
            Ok(wait) if wait_status(&wait) == Some("exited") => {
                return Err(resume_failure(
                    params,
                    OrcaError::Orca {
                        code: "terminal_exited".into(),
                        message: "the agent exited during startup".into(),
                    },
                ));
            }
            Ok(_) => {}
            Err(e) if e.is_wait_timeout() => {
                tracing::warn!(
                    handle,
                    "the agent did not report tui-idle within {STARTUP_WAIT_MS}ms; sending the \
                     prompt anyway"
                );
            }
            Err(e) => return Err(resume_failure(params, e)),
        }
        self.wait_for_agent(params, handle).await?;

        let prompt = compose_prompt(params);
        if prompt.trim().is_empty() {
            return Ok(());
        }
        let sent = self
            .cli
            .run(args([
                "terminal",
                "send",
                "--terminal",
                handle,
                "--text",
                &prompt,
                "--enter",
                "--wait-submit",
                &SUBMIT_WAIT_SECS.to_string(),
                "--json",
            ]))
            .await
            .map_err(|e| resume_failure(params, e))?;
        // A refusal can come back inside a successful envelope. Nothing was
        // typed, so the dispatch fails and its terminal is closed.
        if sent
            .get("send")
            .and_then(|s| s.get("accepted"))
            .and_then(Value::as_bool)
            == Some(false)
        {
            return Err(OrcaError::InvalidResponse(
                "orca did not accept the task prompt (`terminal send` answered accepted: false)"
                    .into(),
            ));
        }
        let prompt_report = sent.get("send").and_then(|s| s.get("prompt"));
        let stages: Vec<&str> = prompt_report
            .and_then(|p| p.get("stages"))
            .and_then(Value::as_array)
            .map(|s| s.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let observable = prompt_report
            .and_then(|p| p.get("observation"))
            .and_then(Value::as_str)
            == Some("supported");
        // Not an error: the text was accepted, and a resend would deliver the
        // task twice. The deadman and the hooks still cover the outcome.
        if observable && !stages.contains(&"turn_started") {
            tracing::warn!(
                handle,
                ?stages,
                "orca accepted the prompt but did not observe a turn start within \
                 {SUBMIT_WAIT_SECS}s"
            );
        }
        Ok(())
    }

    /// Wait until orca recognises the terminal as an agent (`agentIdentity`).
    ///
    /// **`tui-idle` alone is not enough**, measured live: it was satisfied ~4s
    /// after launch while `agentIdentity` was still `null`, and a prompt sent
    /// at that moment went out as `provider: "unsupported"` — raw keystrokes,
    /// no bracketed paste — and never reached Claude. Sent once orca knew the
    /// agent, the same prompt went out as `provider: "claude"` and started a
    /// turn.
    ///
    /// A tool orca has no integration for never gets an identity, so running
    /// out of [`AGENT_DETECT_WAIT`] is a warning and the prompt is sent anyway.
    /// An agent that exits meanwhile is a startup failure.
    async fn wait_for_agent(
        &self,
        params: &TaskDispatchParams,
        handle: &str,
    ) -> Result<(), OrcaError> {
        let deadline = tokio::time::Instant::now() + AGENT_DETECT_WAIT;
        loop {
            match show_terminal(&self.cli, handle).await {
                Ok(t) if !t.connected => {
                    return Err(resume_failure(
                        params,
                        OrcaError::Orca {
                            code: "terminal_exited".into(),
                            message: "the agent exited during startup".into(),
                        },
                    ));
                }
                Ok(t) if t.agent_identity.is_some() => return Ok(()),
                Ok(_) => {}
                Err(e) if e.is_gone() => return Err(resume_failure(params, e)),
                // A transient read failure is not a verdict; keep waiting.
                Err(e) => {
                    tracing::debug!(handle, error = %e, "terminal show failed while waiting for the agent")
                }
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    handle,
                    "orca did not recognise an agent in the terminal within {}s; sending the \
                     prompt anyway",
                    AGENT_DETECT_WAIT.as_secs()
                );
                return Ok(());
            }
            tokio::time::sleep(AGENT_DETECT_POLL).await;
        }
    }

    /// Put the ownership marker on the agent's tab: `terminal rename` to
    /// `totsuka {task_id}`.
    ///
    /// **Not `terminal create --title`**, which is only an initial title —
    /// measured live, Claude's own OSC title (`✳ Claude Code`) replaced it
    /// within seconds, and `session/list` found nothing. A title set with
    /// `rename` is an override orca keeps across the agent's title updates.
    ///
    /// It runs **after** the prompt is in: orca shows `agentIdentity` only
    /// while the tab carries the agent's own title, and
    /// [`wait_for_agent`](Self::wait_for_agent) reads exactly that.
    ///
    /// Best-effort. A tab without the marker costs `session/list` (and so
    /// `doctor`'s orphan detection) this one task; failing a dispatch whose
    /// agent is already working would cost far more.
    async fn mark_owned(&self, params: &TaskDispatchParams, handle: &str) {
        if let Err(e) = self
            .cli
            .run(args([
                "terminal",
                "rename",
                "--terminal",
                handle,
                "--title",
                &owned_title(params),
                "--json",
            ]))
            .await
        {
            tracing::warn!(handle, error = %e, "could not title the agent's tab; session/list will not see it");
        }
    }

    /// Name the task's worktree `{repo}: {title}` in orca's sidebar (herdr's
    /// #417 identity report). Best-effort: a refusal is logged and ignored.
    async fn report_identity(&self, params: &TaskDispatchParams) {
        if !self.config.identity.enabled {
            return;
        }
        let name = display_name(params.repo_name.as_deref(), &params.task.title);
        if let Err(e) = self
            .cli
            .run(args([
                "worktree",
                "set",
                "--worktree",
                &format!("path:{}", params.worktree_path),
                "--display-name",
                &name,
                "--json",
            ]))
            .await
        {
            tracing::warn!(error = %e, "could not set the worktree's orca display name");
        }
    }

    /// Split a companion shell off the agent's terminal (`[orca.layout]`).
    /// Best-effort, like herdr's layout.
    async fn apply_layout(&self, handle: &str) {
        if !self.config.layout.shell {
            return;
        }
        let mut argv = args(["terminal", "split", "--terminal", handle]);
        if let Some(direction) = &self.config.layout.direction {
            argv.extend(args(["--direction", direction.as_str()]));
        }
        argv.push("--json".into());
        if let Err(e) = self.cli.run(argv).await {
            tracing::warn!(error = %e, "could not split a companion shell off the agent");
        }
    }

    /// Close the terminals a `terminal create` without a handle may have left:
    /// those in the task's worktree still carrying the initial title.
    /// Best-effort, like [`abandon`](Self::abandon).
    async fn abandon_untracked(&self, params: &TaskDispatchParams) {
        let title = owned_title(params);
        let listed = match self
            .cli
            .run(args([
                "terminal",
                "list",
                "--worktree",
                &format!("path:{}", params.worktree_path),
                "--json",
            ]))
            .await
        {
            Ok(listed) => listed,
            Err(e) => {
                tracing::warn!(error = %e, "could not look for a terminal created without a handle");
                return;
            }
        };
        let handles = listed
            .get("terminals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|t| t.get("title").and_then(Value::as_str) == Some(title.as_str()))
            .filter_map(|t| t.get("handle").and_then(Value::as_str));
        for handle in handles {
            self.abandon(handle).await;
        }
    }

    /// Close a dispatch's terminal after a failure. Best-effort: the dispatch
    /// error is the answer, and a failed teardown must not replace it.
    async fn abandon(&self, handle: &str) {
        if let Err(e) = close_tab(&self.cli, handle).await
            && !e.is_missing()
        {
            tracing::warn!(handle, error = %e, "could not close an abandoned dispatch's terminal");
        }
    }

    /// Re-attach to a dispatched session (F-37): the terminal must still run
    /// its process. Its state is the worktree's orca status, held at `running`
    /// when orca has none to give.
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, OrcaError> {
        let terminal = match show_terminal(&self.cli, session_id).await {
            Ok(t) => t,
            Err(e) if e.is_missing() => return Ok(detached()),
            Err(e) => return Err(e),
        };
        if !terminal.connected {
            return Ok(detached());
        }
        let status = match &terminal.worktree_path {
            Some(path) => self.worktree_status(path).await,
            None => None,
        };
        Ok(SessionAttachResult {
            attached: true,
            state: status
                .map(|s| map_orca_state(&s, AgentState::Running))
                .unwrap_or(AgentState::Running),
        })
    }

    /// The `worktree ps` status of the worktree at `path`, if orca reports one.
    async fn worktree_status(&self, path: &str) -> Option<String> {
        let ps = self
            .cli
            .run(args([
                "worktree",
                "ps",
                "--limit",
                &LIST_LIMIT.to_string(),
                "--json",
            ]))
            .await
            .ok()?;
        ps.get("worktrees")?
            .as_array()?
            .iter()
            .find(|w| {
                w.get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|p| same_path(p, path))
            })?
            .get("status")?
            .as_str()
            .map(str::to_string)
    }

    /// Cancel a task: close the agent's tab, which kills its process. A
    /// terminal that is already gone counts as success, so cancel is
    /// idempotent. **The worktree is left alone** — it is the Orchestrator's.
    pub async fn cancel(&self, session_id: &str) -> Result<(), OrcaError> {
        match close_tab(&self.cli, session_id).await {
            Ok(()) => Ok(()),
            // `is_gone`, not `is_missing`: a close that races the agent's own
            // exit has nothing left to cancel either.
            Err(e) if e.is_gone() => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Release a **finished** session's terminal (`session/release`, #210).
    ///
    /// Handles are not reused, so the identity guard herdr needs is here only
    /// as a cross-check: a present `expect_*` that disagrees with the terminal
    /// refuses the close, as the protocol asks.
    pub async fn release(
        &self,
        params: &SessionReleaseParams,
    ) -> Result<SessionReleaseResult, OrcaError> {
        let terminal = match show_terminal(&self.cli, &params.session_id).await {
            Ok(t) => t,
            // Nothing at that handle. The task may still hold a live terminal
            // under another one (a later dispatch), which is the case
            // `refused` exists for — so look, as herdr does.
            Err(e) if e.is_missing() => {
                return Ok(not_released(self.classify_unreleased(params).await));
            }
            Err(e) => return Err(e),
        };
        let cwd_mismatch = matches!(
            (params.expect_cwd.as_deref(), terminal.worktree_path.as_deref()),
            (Some(expected), Some(actual)) if !same_path(expected, actual)
        );
        let label_mismatch = matches!(
            (params.expect_label.as_deref(), terminal.title.as_deref()),
            (Some(expected), Some(actual)) if expected != actual
        );
        if cwd_mismatch || label_mismatch {
            tracing::warn!(
                handle = %params.session_id,
                expect_cwd = ?params.expect_cwd,
                actual_cwd = ?terminal.worktree_path,
                "release refused: the terminal is not the task's"
            );
            return Ok(not_released(self.classify_unreleased(params).await));
        }
        if !terminal.connected {
            // The process already exited; the tab is only a leftover surface.
            // Tidy it, and answer that there was nothing live to release.
            if let Err(e) = close_tab(&self.cli, &params.session_id).await
                && !e.is_missing()
            {
                tracing::warn!(error = %e, "could not close an exited agent's tab");
            }
            return Ok(not_released(NotReleased::Gone));
        }
        match close_tab(&self.cli, &params.session_id).await {
            Ok(()) => Ok(SessionReleaseResult {
                released: true,
                not_released: None,
            }),
            Err(e) if e.is_missing() => Ok(not_released(NotReleased::Gone)),
            Err(e) => Err(e),
        }
    }

    /// Whether the task still holds a live terminal of ours, when the recorded
    /// handle did not resolve to it (0.4.2, #485). Two pieces of evidence,
    /// either of which is enough: a live owned terminal in the expected
    /// worktree (`expect_cwd`, what the worktree cleanup sends), or one
    /// carrying the expected label (`expect_label`, what `doctor` sends — its
    /// `totsuka {task_id}` is exactly the tab title). No evidence degrades to
    /// `Gone`.
    async fn classify_unreleased(&self, params: &SessionReleaseParams) -> NotReleased {
        let cwd = params.expect_cwd.as_deref();
        let label = params.expect_label.as_deref();
        if cwd.is_none() && label.is_none() {
            return NotReleased::Gone;
        }
        match self.list_sessions().await {
            Ok(list)
                if list.sessions.iter().any(|s| {
                    s.session_id != params.session_id
                        && (cwd
                            .is_some_and(|cwd| s.cwd.as_deref().is_some_and(|c| same_path(c, cwd)))
                            || (label.is_some() && s.label.as_deref() == label))
                }) =>
            {
                NotReleased::Refused
            }
            Ok(_) => NotReleased::Gone,
            Err(e) => {
                tracing::warn!(error = %e, "could not list terminals to classify an unreleased session");
                NotReleased::Gone
            }
        }
    }

    /// Enumerate the live terminals this plugin owns (`session/list`, #211):
    /// those whose tab title carries the `totsuka ` marker. A companion shell
    /// split off the agent has no title of its own, so each task is listed
    /// once, by its agent's handle.
    pub async fn list_sessions(&self) -> Result<SessionListResult, OrcaError> {
        let listed = self
            .cli
            .run(args([
                "terminal",
                "list",
                "--limit",
                &LIST_LIMIT.to_string(),
                "--json",
            ]))
            .await?;
        let sessions = listed
            .get("terminals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let title = t.get("title").and_then(Value::as_str)?;
                if !title.starts_with(OWNED_TITLE_PREFIX) {
                    return None;
                }
                // `terminal list` shows live terminals only, but say so
                // explicitly rather than depend on it.
                if t.get("connected").and_then(Value::as_bool) == Some(false) {
                    return None;
                }
                Some(SessionInfo {
                    session_id: t.get("handle").and_then(Value::as_str)?.to_string(),
                    label: Some(title.to_string()),
                    cwd: t
                        .get("worktreePath")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect();
        Ok(SessionListResult { sessions })
    }

    /// Bring the session's terminal to the foreground (`session/focus`, F-94).
    /// A terminal that is gone or exited reports `focused: false`.
    pub async fn focus(&self, session_id: &str) -> Result<SessionFocusResult, OrcaError> {
        match self
            .cli
            .run(args([
                "terminal",
                "switch",
                "--terminal",
                session_id,
                "--json",
            ]))
            .await
        {
            Ok(_) => Ok(SessionFocusResult { focused: true }),
            Err(e) if e.is_gone() => Ok(SessionFocusResult { focused: false }),
            Err(e) => Err(e),
        }
    }

    /// Capture the terminal's screen (`diagnostics/snapshot`, R-10): the
    /// rendered screen, falling back to the accumulated output when orca
    /// cannot render one. Never an error — no text is `None`.
    pub async fn snapshot(&self, session_id: &str) -> Result<DiagnosticsSnapshotResult, OrcaError> {
        let screen = self
            .read_text(args([
                "terminal",
                "read",
                "--terminal",
                session_id,
                "--screen",
                "--json",
            ]))
            .await;
        let text = match screen {
            Some(text) => Some(text),
            None => {
                self.read_text(args([
                    "terminal",
                    "read",
                    "--terminal",
                    session_id,
                    "--limit",
                    &SNAPSHOT_LINES.to_string(),
                    "--json",
                ]))
                .await
            }
        };
        Ok(DiagnosticsSnapshotResult { text })
    }

    /// One `terminal read`'s `tail`, joined. `None` for an error, an empty
    /// read, or a screen orca could not render.
    async fn read_text(&self, argv: Vec<String>) -> Option<String> {
        let read = self.cli.run(argv).await.ok()?;
        let terminal = read.get("terminal")?;
        if terminal.get("source").and_then(Value::as_str) == Some("screen-unavailable") {
            return None;
        }
        let text = terminal
            .get("tail")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// The state stream for a session (F-38), a **deadman** like herdr's
    /// since completion is reported by the agent's hooks (#131): block on
    /// `terminal wait --for exit` and report `failed` when the agent's process
    /// goes away.
    ///
    /// **Every exit is a failure**, where herdr lets an explicit exit code 0
    /// pass silently. orca's `exitCode` cannot carry that distinction:
    /// measured, a process that exited 3 was reported as `exitCode: 0` with
    /// `exitCause: unknown`. It costs nothing in practice — an interactive
    /// agent does not exit when it finishes, and a notification for a task the
    /// hooks already completed is ignored by the Orchestrator.
    pub async fn start_state_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, OrcaError> {
        let cli = self.cli.clone();
        let session_id = session_id.to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut consecutive_errors = 0u32;
            let reason = loop {
                if tx.is_closed() {
                    return; // the consumer is gone
                }
                let waited = cli
                    .run(args([
                        "terminal",
                        "wait",
                        "--terminal",
                        &session_id,
                        "--for",
                        "exit",
                        "--timeout-ms",
                        &EXIT_WAIT_MS.to_string(),
                        "--json",
                    ]))
                    .await;
                match waited {
                    Ok(wait) if wait_satisfied(&wait) => break exit_description(&wait),
                    Ok(_) => consecutive_errors = 0,
                    Err(e) if e.is_wait_timeout() => consecutive_errors = 0,
                    Err(e) if e.is_gone() => {
                        break "the agent's terminal is gone (closed, or its process exited)"
                            .to_string();
                    }
                    Err(e) => {
                        consecutive_errors += 1;
                        tracing::warn!(error = %e, consecutive_errors, "terminal wait failed");
                        if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                            break format!(
                                "orca could not be asked about the agent's terminal after \
                                 {consecutive_errors} attempts: {e}"
                            );
                        }
                        tokio::time::sleep(ERROR_BACKOFF).await;
                    }
                }
            };
            let _ = tx.send(StateNotification {
                session_id,
                state: AgentState::Failed,
                log_chunk: Some(reason),
            });
        });

        Ok(rx)
    }
}

/// The fields of a `terminal show` this plugin reads.
struct TerminalRecord {
    connected: bool,
    /// The agent orca recognises in the terminal (`claude`, …), if any.
    agent_identity: Option<String>,
    worktree_path: Option<String>,
    title: Option<String>,
}

/// `terminal show` for `handle`.
async fn show_terminal<C: OrcaCli>(cli: &C, handle: &str) -> Result<TerminalRecord, OrcaError> {
    let shown = cli
        .run(args(["terminal", "show", "--terminal", handle, "--json"]))
        .await?;
    let terminal = shown
        .get("terminal")
        .ok_or_else(|| OrcaError::InvalidResponse("`terminal show` returned no terminal".into()))?;
    let text = |key: &str| {
        terminal
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Ok(TerminalRecord {
        // Absent means an orca that does not say; a live record is the safer
        // reading, since `terminal show` answered at all.
        connected: terminal
            .get("connected")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        agent_identity: text("agentIdentity"),
        worktree_path: text("worktreePath"),
        title: text("title"),
    })
}

/// Close `handle`'s whole tab — the agent and any companion shell split off it.
async fn close_tab<C: OrcaCli>(cli: &C, handle: &str) -> Result<(), OrcaError> {
    cli.run(args([
        "terminal",
        "close",
        "--terminal",
        handle,
        "--tab",
        "--json",
    ]))
    .await
    .map(|_| ())
}

/// `wait.status` of a `terminal wait` result.
fn wait_status(wait: &Value) -> Option<&str> {
    wait.get("wait")?.get("status")?.as_str()
}

/// Whether a `terminal wait --for exit` saw the exit.
fn wait_satisfied(wait: &Value) -> bool {
    let inner = wait.get("wait");
    inner
        .and_then(|w| w.get("satisfied"))
        .and_then(Value::as_bool)
        == Some(true)
        || wait_status(wait) == Some("exited")
}

/// A log line for an observed exit, with whatever orca said about it.
fn exit_description(wait: &Value) -> String {
    let inner = wait.get("wait");
    let code = inner
        .and_then(|w| w.get("exitCode"))
        .map(Value::to_string)
        .unwrap_or_else(|| "unknown".into());
    let cause = inner
        .and_then(|w| w.get("exitCause"))
        .and_then(|c| c.get("reason").or_else(|| c.get("kind")))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("the agent's terminal exited (orca reports exit code {code}, cause {cause})")
}

/// Whether two paths name the same directory: equal as given, or equal once
/// resolved (`/tmp` vs `/private/tmp` on macOS).
fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let resolve = |p: &str| std::fs::canonicalize(Path::new(p)).ok();
    matches!((resolve(a), resolve(b)), (Some(x), Some(y)) if x == y)
}

fn detached() -> SessionAttachResult {
    SessionAttachResult {
        attached: false,
        state: AgentState::Failed,
    }
}

fn not_released(why: NotReleased) -> SessionReleaseResult {
    SessionReleaseResult {
        released: false,
        not_released: Some(why),
    }
}

/// A failure while bringing a **resumed** session up, where the terminal is
/// gone, is reported as [`OrcaError::SessionUnresumable`] → the protocol's
/// `SESSION_UNRESUMABLE`, so the Orchestrator retries once without the
/// session (#242). `claude --resume <unknown id>` prints an error and exits,
/// and `exec` makes that exit the terminal's. Anything else keeps its own
/// error (the same narrowness as herdr's `resume_failure`).
fn resume_failure(params: &TaskDispatchParams, error: OrcaError) -> OrcaError {
    if params.resume_session_id.is_some() && error.is_gone() {
        return OrcaError::SessionUnresumable(error.to_string());
    }
    error
}

/// Compose the agent prompt: any extra context as a preamble, then the task
/// (body, or the title when there is no body) — the herdr plugin's layout.
///
/// A string `extra_context` is rendered as raw text, not as a JSON literal
/// (#158). Hook-capable dispatches usually carry none: the instructions ride
/// the `UserPromptSubmit` hook instead.
fn compose_prompt(params: &TaskDispatchParams) -> String {
    let task_text = params.task.body.as_ref().unwrap_or(&params.task.title);
    match &params.extra_context {
        Some(Value::String(s)) => format!("{s}\n\n---\n{task_text}"),
        Some(other) => format!("{other}\n\n---\n{task_text}"),
        None => task_text.clone(),
    }
}

/// The worktree's display name: `{repo}: {title}`, whitespace collapsed and
/// cut to [`DISPLAY_NAME_CHARS`] characters on a char boundary (task titles
/// here are often Japanese, so a byte slice would panic).
fn display_name(repo: Option<&str>, title: &str) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let full = match repo {
        Some(repo) => format!("{repo}: {title}"),
        None => title,
    };
    if full.chars().count() <= DISPLAY_NAME_CHARS {
        return full;
    }
    let cut: String = full.chars().take(DISPLAY_NAME_CHARS - 1).collect();
    format!("{cut}…")
}

/// The tab title that marks a terminal as this task's.
fn owned_title(params: &TaskDispatchParams) -> String {
    format!("{OWNED_TITLE_PREFIX}{}", params.task.id)
}

/// An owned argv from string slices.
fn args<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(body: Option<&str>, extra: Option<Value>) -> TaskDispatchParams {
        TaskDispatchParams {
            task: plugin_protocol::task::Task {
                id: "t1".into(),
                source: "slack".into(),
                title: "title".into(),
                body: body.map(str::to_string),
                repo_hint: None,
                labels: vec![],
                priority: 0,
                status: None,
                url: None,
                assignee: None,
                message_key: None,
                instructions: None,
                handle: None,
            },
            worktree_path: "/wt".into(),
            mode: plugin_protocol::methods::ExecutionMode::Plan,
            extra_context: extra,
            job_id: None,
            task_number: None,
            resume_session_id: None,
            tool_launch: None,
            repo_name: None,
        }
    }

    #[test]
    fn compose_prompt_puts_string_extra_context_first_as_raw_text() {
        let prompt = compose_prompt(&params(
            Some("body"),
            Some(Value::String("line one\nline two".into())),
        ));
        assert_eq!(prompt, "line one\nline two\n\n---\nbody");
        assert!(!prompt.contains("\\n"), "no JSON escapes: {prompt}");
    }

    #[test]
    fn compose_prompt_falls_back_to_the_title() {
        assert_eq!(compose_prompt(&params(None, None)), "title");
    }

    #[test]
    fn display_name_is_cut_on_a_char_boundary() {
        assert_eq!(
            display_name(Some("web"), "fix  the\nbug"),
            "web: fix the bug"
        );
        let long = "あ".repeat(100);
        let name = display_name(None, &long);
        assert_eq!(name.chars().count(), DISPLAY_NAME_CHARS);
        assert!(name.ends_with('…'));
    }

    #[test]
    fn exit_description_names_what_orca_said() {
        let wait = json!({ "wait": {
            "satisfied": true, "status": "exited", "exitCode": 0,
            "exitCause": { "kind": "unknown", "reason": "host_status_unavailable" }
        }});
        assert!(wait_satisfied(&wait));
        let text = exit_description(&wait);
        assert!(text.contains("exit code 0"), "{text}");
        assert!(text.contains("host_status_unavailable"), "{text}");
    }

    #[test]
    fn a_running_tui_idle_wait_is_not_an_exit() {
        let wait =
            json!({ "wait": { "condition": "tui-idle", "satisfied": true, "status": "running" }});
        assert_eq!(wait_status(&wait), Some("running"));
    }

    #[test]
    fn resume_failure_is_narrow() {
        let gone = || OrcaError::Orca {
            code: "terminal_exited".into(),
            message: String::new(),
        };
        let mut p = params(None, None);
        assert!(matches!(resume_failure(&p, gone()), OrcaError::Orca { .. }));
        p.resume_session_id = Some("sid".into());
        assert!(matches!(
            resume_failure(&p, gone()),
            OrcaError::SessionUnresumable(_)
        ));
        let other = OrcaError::InvalidResponse("x".into());
        assert!(matches!(
            resume_failure(&p, other),
            OrcaError::InvalidResponse(_)
        ));
    }
}
