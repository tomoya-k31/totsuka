//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.
//!
//! Protocol facts this adapter is written against (herdr 0.7.4, verified live
//! against real Claude Code in #124, mirrored in
//! `docs/references/herdr-socket-api.md`):
//! - the agent CLI is launched with `agent.start {name, argv, cwd, workspace_id}`
//!   (`workspace.create` has no command params). Both methods accept an optional
//!   `env` map (herdr 0.7.1+), used to inject the Orchestrator's hook
//!   environment (`TOTSUKA_JOB_ID`, …) when a [`HookLaunchSpec`] is supplied
//! - the prompt **cannot** ride in `argv`: a multi-line prompt passed that way
//!   is never submitted, and every task body here is multi-line. It is typed in
//!   with `agent.send` and submitted with Enter — both confirmed, never
//!   fire-and-forget, because the CLI accepts keystrokes before it acts on them
//!   (see [`HerdrAgent::submit_prompt`])
//! - events arrive as `{event, data}` envelopes whose kind separator is
//!   inconsistent (`pane.agent_status_changed` but `pane_exited`), so kinds are
//!   compared normalized
//!
//! # Completion detection is now hook-based (0.1.3, #131 / R-07)
//!
//! Task completion is reported out-of-band by Claude Code's Stop/SessionEnd
//! hooks (POST to the Orchestrator's UDS), **not** by this plugin's state
//! stream. So the screen-manifest completion path (mapping `working → idle`,
//! confirming a debounced `done`, scraping the answer off the pane/transcript,
//! extracting a `waiting_input` question) is **removed**. The state stream is
//! reduced to a **deadman**: it subscribes only to `pane.exited` and reports
//! `Failed` on an abnormal exit — the hook already reported a normal end. This
//! reduction is unconditional (it holds even when no hook spec is supplied); an
//! orchestrator older than 0.1.3 will therefore not learn of completion, which
//! `initialize` warns about.

use plugin_protocol::methods::{
    AgentState, DiagnosticsSnapshotResult, ExecutionMode, SessionAttachResult, SessionFocusResult,
    StateNotification, TaskDispatchParams, TaskDispatchResult,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, map_agent_status, squash_ws};
use crate::transport::{HerdrTransport, SUBSCRIPTION_CLOSED_EVENT};

/// How many screen lines are read when extracting text from a pane.
const SCREEN_LINES: u64 = 200;

/// How many times the prompt is typed in before giving up, and how long each
/// attempt waits for it to appear on screen.
const SEND_ATTEMPTS: usize = 5;
const SEND_RENDER_TIMEOUT: Duration = Duration::from_secs(3);

/// How many times Enter is pressed before giving up, and how long each press is
/// given to start the agent.
const ENTER_ATTEMPTS: usize = 10;
const ENTER_SETTLE: Duration = Duration::from_millis(1200);

/// How long a screen check waits between polls.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How much of the prompt's tail identifies it on screen. The input box scrolls
/// with the cursor, so a long prompt shows its **end** — matching the head
/// would fail exactly when the prompt is long.
const PROMPT_MARKER_CHARS: usize = 24;

/// The herdr agent_ide adapter, generic over its [`HerdrTransport`].
pub struct HerdrAgent<T> {
    client: T,
    config: HerdrConfig,
}

impl<T: HerdrTransport> HerdrAgent<T> {
    /// A new adapter over `client` using `config`.
    pub fn new(client: T, config: HerdrConfig) -> Self {
        Self { client, config }
    }

    /// Dispatch a task (F-31/F-37): create a herdr workspace on the worktree,
    /// start the agent CLI in it (plan mode when asked, F-36), submit the task
    /// prompt, and return a `(pane_id, agent_session_id)` re-attach handle as
    /// the session id.
    ///
    /// Returns only once the agent has actually started working, so a caller
    /// that subscribes afterwards can trust the pane's status (see
    /// [`start_state_stream`](Self::start_state_stream)).
    pub async fn dispatch(
        &self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, HerdrError> {
        let plan = params.mode == ExecutionMode::Plan;
        // The hook launch spec (0.1.3) is opaque to the plugin: `--settings`
        // wires Claude Code's hooks, `env` supplies the hook environment
        // (`TOTSUKA_JOB_ID`, …). `--resume` re-opens a past agent session for
        // Slack thread continuation.
        let hook_settings = params.hook.as_ref().map(|h| h.settings_path.as_str());
        let resume = params.resume_session_id.as_deref();
        let (program, args) = self.config.launch_command(plan, hook_settings, resume);
        // herdr injects this env into the launched process (workspace.create +
        // agent.start both take `env`). Only set when a hook spec is present.
        let env: Option<Value> = params.hook.as_ref().map(|h| json!(h.env));

        // A workspace per task keeps agent panes out of the operator's own
        // workspaces (there is no "start session" method; the workspace is the
        // container).
        let mut create_params = json!({
            "cwd": params.worktree_path,
            "label": format!("totsuka {}", params.task.id),
        });
        if let Some(env) = &env {
            create_params["env"] = env.clone();
        }
        let created = self.client.call("workspace.create", create_params).await?;
        let workspace_id = created
            .get("workspace")
            .and_then(|w| w.get("workspace_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HerdrError::InvalidResponse("workspace.create returned no workspace_id".into())
            })?;

        // From here on the workspace exists, so every failure path has to take
        // it back down: a failed dispatch reports no session id, which leaves
        // the Orchestrator no handle to cancel with — the pane and its CLI
        // process would run until the operator noticed them (and `task retry`
        // would strand another one).
        let started = self
            .start_agent(&params, workspace_id, program, args, env.as_ref())
            .await
            .inspect_err(|_| self.abandon(workspace_id))?;
        Ok(started)
    }

    /// The part of [`dispatch`](Self::dispatch) that runs with a workspace
    /// allocated: start the CLI, submit the prompt, and build the handle.
    async fn start_agent(
        &self,
        params: &TaskDispatchParams,
        workspace_id: &str,
        program: String,
        args: Vec<String>,
        env: Option<&Value>,
    ) -> Result<TaskDispatchResult, HerdrError> {
        let argv: Vec<String> = std::iter::once(program).chain(args).collect();
        let mut start_params = json!({
            "name": format!("totsuka {}", params.task.id),
            "argv": argv,
            "cwd": params.worktree_path,
            "workspace_id": workspace_id,
            "focus": false,
        });
        if let Some(env) = env {
            start_params["env"] = env.clone();
        }
        let started = self.client.call("agent.start", start_params).await?;
        let agent = started.get("agent").unwrap_or(&started);
        let pane_id = agent
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| HerdrError::InvalidResponse("agent.start returned no pane_id".into()))?
            .to_string();

        self.submit_prompt(&pane_id, &compose_prompt(params))
            .await?;

        // The agent's own session id (for `claude --resume`) is reported by its
        // herdr integration hook during startup; by now it is normally there,
        // and an empty one only degrades resume.
        let agent_session_id = pane_record(&self.client, &pane_id)
            .await
            .ok()
            .and_then(|pane| agent_session_id(&pane))
            .unwrap_or_default();

        let handle = SessionHandle::new(pane_id, agent_session_id);
        Ok(TaskDispatchResult {
            session_id: handle.encode(),
        })
    }

    /// Tear down a workspace a failed dispatch allocated, best-effort: the
    /// dispatch error is what the caller needs to see, so a cleanup that also
    /// fails is logged rather than raised.
    fn abandon(&self, workspace_id: &str) {
        let client = self.client.clone();
        let workspace_id = workspace_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = client
                .call("workspace.close", json!({ "workspace_id": workspace_id }))
                .await
            {
                tracing::warn!(
                    workspace_id, error = %e,
                    "could not close the workspace of a failed dispatch; it may need closing by hand"
                );
            }
        });
    }

    /// Type `prompt` into the agent's TUI and submit it, confirming both steps.
    ///
    /// Neither write can be fire-and-forget (#124): the CLI renders its input
    /// box before it accepts input, and accepts input before it acts on Enter,
    /// so text sent too early is dropped and an early Enter is swallowed —
    /// leaving a pane that sits idle forever with the prompt unsent. Instead the
    /// text is re-sent until it shows up on screen, and Enter is re-pressed
    /// until the agent actually starts. Both retries are safe: `agent.send`
    /// replaces nothing (the guard re-checks the screen first) and Enter on an
    /// empty input box is a no-op.
    async fn submit_prompt(&self, pane_id: &str, prompt: &str) -> Result<(), HerdrError> {
        let marker = prompt_marker(prompt);
        // Retrying past herdr errors is what absorbs a blip — but it also hides
        // a socket that is simply down, so the last one is kept and reported
        // with the failure. Without it the caller only learns "it never
        // started", never that herdr was unreachable all along.
        let mut last_error: Option<HerdrError> = None;

        let mut typed = false;
        for _ in 0..SEND_ATTEMPTS {
            if self.screen_contains(pane_id, &marker).await {
                typed = true;
                break;
            }
            // A blip on the way in is what the retries are for; only a pane
            // that is truly gone ends this early.
            if let Err(e) = self
                .client
                .call("agent.send", json!({ "target": pane_id, "text": prompt }))
                .await
            {
                if e.is_missing() {
                    return Err(e);
                }
                tracing::warn!(pane_id, error = %e, "agent.send failed; retrying");
                last_error = Some(e);
            }
            if self
                .wait_for(SEND_RENDER_TIMEOUT, || {
                    self.screen_contains(pane_id, &marker)
                })
                .await
            {
                typed = true;
                break;
            }
            tracing::warn!(
                pane_id,
                "the prompt did not reach the agent's input box; retrying"
            );
        }
        if !typed {
            return Err(gave_up(
                format!("the agent CLI never showed the prompt in pane {pane_id}"),
                last_error,
            ));
        }

        for _ in 0..=ENTER_ATTEMPTS {
            match self.agent_is_running(pane_id).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) if e.is_missing() => return Err(e),
                Err(e) => {
                    tracing::warn!(pane_id, error = %e, "could not read the pane's status; retrying");
                    last_error = Some(e);
                }
            }
            if let Err(e) = self
                .client
                .call(
                    "pane.send_keys",
                    json!({ "pane_id": pane_id, "keys": ["enter"] }),
                )
                .await
            {
                if e.is_missing() {
                    return Err(e);
                }
                tracing::warn!(pane_id, error = %e, "Enter failed; retrying");
                last_error = Some(e);
            }
            tokio::time::sleep(ENTER_SETTLE).await;
        }
        Err(gave_up(
            format!("the agent in pane {pane_id} never started after the prompt was submitted"),
            last_error,
        ))
    }

    /// Whether the agent has acted on the prompt.
    async fn agent_is_running(&self, pane_id: &str) -> Result<bool, HerdrError> {
        pane_status(&self.client, pane_id)
            .await
            .map(|status| agent_started(&status))
    }

    /// Re-attach to a dispatched session (F-37): confirm the pane is alive with
    /// `pane.get` and report its current mapped state. A vanished pane
    /// (`pane_not_found`) is reported as `attached: false` (the Orchestrator's
    /// recovery then defers to a human).
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        match pane_status(&self.client, &handle.pane_id).await {
            Ok(status) => Ok(SessionAttachResult {
                attached: true,
                state: map_agent_status(&status, AgentState::Idle),
            }),
            // The pane is gone → not attached (F-37); the Orchestrator (#57)
            // routes this to human confirmation rather than auto-failing.
            Err(e) if e.is_missing() => Ok(SessionAttachResult {
                attached: false,
                state: AgentState::Failed,
            }),
            Err(e) => Err(e),
        }
    }

    /// Cancel a task: interrupt the agent (`ctrl+c`), then take down the
    /// workspace `dispatch` created for it. The interrupt is best-effort — even
    /// if it fails the pane is still closed — and anything already gone counts
    /// as success, so cancel is idempotent.
    pub async fn cancel(&self, session_id: &str) -> Result<(), HerdrError> {
        let handle = SessionHandle::decode(session_id);
        // Best-effort interrupt: a failure here must not prevent the close,
        // otherwise a stuck agent's pane would leak.
        if let Err(e) = self
            .client
            .call(
                "pane.send_keys",
                json!({ "pane_id": handle.pane_id, "keys": ["ctrl+c"] }),
            )
            .await
            && !e.is_missing()
        {
            tracing::warn!(error = %e, "pane.send_keys failed during cancel; closing anyway");
        }
        ignore_missing(
            self.client
                .call("pane.close", json!({ "pane_id": handle.pane_id }))
                .await,
        )?;
        // `dispatch` gives every task its own workspace, so closing the pane
        // alone would leave an empty one behind on every cancel.
        if let Some(workspace_id) = workspace_of(&handle.pane_id) {
            ignore_missing(
                self.client
                    .call("workspace.close", json!({ "workspace_id": workspace_id }))
                    .await,
            )?;
        }
        Ok(())
    }

    /// Bring the session's pane to the foreground (`session/focus`, F-94):
    /// confirm the pane is alive with `pane.get`, then focus outside-in —
    /// workspace, tab, pane — so herdr lands on the right pane whichever
    /// container was active. The GUI terminal itself is brought to the front
    /// separately by the notifier (`-activate`); this only moves herdr's
    /// focus. A vanished pane (or container) reports `focused: false` — a
    /// notification clicked after the task's pane closed is a normal path,
    /// not an error.
    pub async fn focus(&self, session_id: &str) -> Result<SessionFocusResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane = match pane_record(&self.client, &handle.pane_id).await {
            Ok(pane) => pane,
            Err(e) if e.is_missing() => return Ok(SessionFocusResult { focused: false }),
            Err(e) => return Err(e),
        };
        // Container ids come from the pane record; the workspace also falls
        // back to the pane-id prefix (`w1:p2` lives in `w1`). A record without
        // a tab id just skips the tab step — `pane.focus` still lands.
        let workspace_id = pane
            .get("workspace_id")
            .and_then(Value::as_str)
            .or_else(|| workspace_of(&handle.pane_id));
        if let Some(workspace_id) = workspace_id
            && !self
                .focus_step("workspace.focus", json!({ "workspace_id": workspace_id }))
                .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        if let Some(tab_id) = pane.get("tab_id").and_then(Value::as_str)
            && !self
                .focus_step("tab.focus", json!({ "tab_id": tab_id }))
                .await?
        {
            return Ok(SessionFocusResult { focused: false });
        }
        let focused = self
            .focus_step("pane.focus", json!({ "pane_id": handle.pane_id }))
            .await?;
        Ok(SessionFocusResult { focused })
    }

    /// One focus call: `Ok(true)` on success, `Ok(false)` when the target is
    /// gone (the pane/tab/workspace closed between the liveness check and this
    /// call), and the error otherwise.
    async fn focus_step(&self, method: &str, params: Value) -> Result<bool, HerdrError> {
        match self.client.call(method, params).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_missing() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Capture the pane's screen for timeout/escalation diagnostics
    /// (`diagnostics/snapshot`, R-10). Reads the `recent` screen copy; a pane
    /// that is gone (or otherwise unreadable) is **not** an error — it reports
    /// `text: None`, so the Orchestrator's escalation path never fails on a
    /// snapshot it could not take.
    pub async fn snapshot(
        &self,
        session_id: &str,
    ) -> Result<DiagnosticsSnapshotResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let text = read_pane_text(&self.client, &handle.pane_id, "recent", SCREEN_LINES).await;
        Ok(DiagnosticsSnapshotResult { text })
    }

    /// Start the state stream for a session (F-38), reduced to a **deadman**
    /// since completion is now reported by Claude Code's hooks (#131 / R-07):
    /// subscribe to `pane.exited` only and report `Failed` on an abnormal exit.
    /// A normal exit (code 0) is silent — the SessionEnd hook already reported
    /// completion out-of-band. The stream ends at the first terminal event.
    pub async fn start_state_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane_id = handle.pane_id.clone();
        // Only the deadman subscription remains — no `pane.agent_status_changed`,
        // so screen-manifest status flicker can no longer drive task state.
        let subscriptions = json!([
            { "type": "pane.exited", "pane_id": pane_id },
        ]);
        // Take the event receiver *before* subscribing, so events herdr pushes
        // immediately after the ACK are buffered rather than raced past.
        let mut events = self.client.events();
        self.client.subscribe_events(subscriptions).await?;

        let session_id = session_id.to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                let signal = match events.recv().await {
                    Ok(event) => classify_exit(&event, &pane_id),
                    // Lagged past the buffer: the deadman only ever emits one
                    // terminal event, so a dropped batch cannot strand a
                    // healthy stream — keep waiting for the exit.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "herdr event stream lagged; deadman still waiting");
                        continue;
                    }
                    // The transport closed: end the stream.
                    Err(_) => break,
                };
                match signal {
                    ExitSignal::Failed => {
                        let _ = tx.send(StateNotification {
                            session_id: session_id.clone(),
                            state: AgentState::Failed,
                            log_chunk: None,
                        });
                        break;
                    }
                    // A clean exit needs no notification (the hook reported the
                    // normal end); the pane is gone, so end the stream.
                    ExitSignal::CleanExit => break,
                    ExitSignal::Ignore => continue,
                }
            }
        });

        Ok(rx)
    }

    /// Whether the pane's screen currently shows `marker` (whitespace-insensitive).
    async fn screen_contains(&self, pane_id: &str, marker: &str) -> bool {
        match read_pane_text(&self.client, pane_id, "visible", SCREEN_LINES).await {
            Some(text) => squash_ws(&text).contains(marker),
            None => false,
        }
    }

    /// Poll `check` until it holds or `timeout` elapses.
    async fn wait_for<F, Fut>(&self, timeout: Duration, check: F) -> bool
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if check().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The pane record (`pane.get` nests it under `result.pane`).
async fn pane_record<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<Value, HerdrError> {
    let result = client
        .call("pane.get", json!({ "pane_id": pane_id }))
        .await?;
    Ok(result.get("pane").cloned().unwrap_or(result))
}

/// The pane's current `agent_status`.
async fn pane_status<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<String, HerdrError> {
    let pane = pane_record(client, pane_id).await?;
    Ok(pane
        .get("agent_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// Whether a herdr `agent_status` means the agent acted on the prompt.
fn agent_started(status: &str) -> bool {
    matches!(status, "working" | "blocked" | "done")
}

/// The workspace a pane belongs to. herdr ids nest the workspace in the pane
/// (`w1:p2` lives in `w1`), which is the only handle back to it — the protocol
/// `session_id` carries the pane, not the workspace.
fn workspace_of(pane_id: &str) -> Option<&str> {
    pane_id.split_once(':').map(|(workspace, _)| workspace)
}

/// The error for a step that exhausted its retries, carrying whatever herdr
/// last complained about. The retries exist to ride out a blip, so the symptom
/// alone ("it never started") is what a caller would otherwise see even when
/// the real story is that the socket was down the whole time.
fn gave_up(symptom: String, cause: Option<HerdrError>) -> HerdrError {
    match cause {
        Some(cause) => {
            HerdrError::InvalidResponse(format!("{symptom} → last herdr error: {cause}"))
        }
        None => HerdrError::InvalidResponse(symptom),
    }
}

/// The agent's native session id from a pane record
/// (`pane.agent_session.value`), reported by the agent's herdr integration hook
/// during startup and carried in the [`SessionHandle`] for `claude --resume`.
/// Absent or empty → `None`.
fn agent_session_id(pane: &Value) -> Option<String> {
    let value = pane
        .get("agent_session")
        .and_then(|s| s.get("value"))
        .and_then(Value::as_str)?;
    (!value.is_empty()).then(|| value.to_string())
}

/// What a single herdr event means to the deadman stream.
enum ExitSignal {
    /// An abnormal exit (nonzero/absent code) or a dead subscription → `Failed`.
    Failed,
    /// A clean exit (code 0) — the SessionEnd hook already reported the normal
    /// end, so the stream ends without a notification.
    CleanExit,
    /// Not for this pane, or not an exit event: ignore.
    Ignore,
}

/// Classify a herdr event envelope for the deadman stream.
///
/// Events arrive as `{event: "<kind>", data: {pane_id, …}}`. Every envelope —
/// herdr's own and the transport's synthetic close — is filtered by pane first:
/// one broadcast carries every subscription in the process, and herdr replays
/// other panes' history on connect, so an unfiltered event would fail a healthy
/// task.
///
/// A `pane_exited` with an explicit `exit_code: 0` is a clean exit (silent); any
/// other exit — nonzero, or no code at all (herdr 0.7.x carries none, so we
/// cannot confirm it was clean) — is `Failed`. In interactive mode Claude Code
/// does not exit on completion, so an unexplained exit really is abnormal.
fn classify_exit(event: &Value, pane_id: &str) -> ExitSignal {
    let Some(kind) = event.get("event").and_then(Value::as_str) else {
        return ExitSignal::Ignore;
    };
    let Some(data) = event.get("data") else {
        return ExitSignal::Ignore;
    };
    if data.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
        return ExitSignal::Ignore;
    }
    // herdr is inconsistent about the separator in event kinds
    // (`pane.agent_status_changed` but `pane_exited`), so compare normalized.
    match kind.replace('.', "_").as_str() {
        "pane_exited" => match data.get("exit_code").and_then(Value::as_i64) {
            Some(0) => ExitSignal::CleanExit,
            _ => ExitSignal::Failed,
        },
        // The subscription connection died; deliver a failure rather than hang
        // a stream that will never see another event. (The pane may still be
        // alive — recovery re-attaches, F-37.)
        SUBSCRIPTION_CLOSED_EVENT => ExitSignal::Failed,
        _ => ExitSignal::Ignore,
    }
}

/// `pane.read` helper: `result.read.text`, ANSI-stripped.
async fn read_pane_text<T: HerdrTransport>(
    client: &T,
    pane_id: &str,
    source: &str,
    lines: u64,
) -> Option<String> {
    let result = client
        .call(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": source,
                "lines": lines,
                "strip_ansi": true,
            }),
        )
        .await
        .ok()?;
    result
        .get("read")
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The whitespace-squashed tail of `prompt`, used to recognize it on screen.
/// Squashed because the input box wraps long lines (and CJK wraps mid-word), so
/// the raw text is never on one line.
fn prompt_marker(prompt: &str) -> String {
    let squashed = squash_ws(prompt);
    let start = squashed
        .char_indices()
        .rev()
        .nth(PROMPT_MARKER_CHARS - 1)
        .map(|(i, _)| i)
        .unwrap_or(0);
    squashed[start..].to_string()
}

/// Compose the agent prompt: any extra context as a preamble, then the task
/// (body, or the title when there is no body).
///
/// The title is typed only when there is no body: sources truncate it to a
/// snippet (e.g. Slack's `TITLE_SNIPPET_CHARS`) and the body carries the full
/// task text, so prepending the title would just show a cut-off duplicate line
/// in the pane. A string `extra_context` is rendered as raw text (not JSON — no
/// surrounding quotes); non-string values keep their JSON rendering.
///
/// The extra context comes FIRST: [`submit_prompt`](HerdrAgent::submit_prompt)
/// confirms arrival by matching the prompt's **tail** on screen
/// ([`prompt_marker`]), and the orchestrator's extra context is a constant
/// instruction — as a suffix it would make every dispatch's tail identical, so
/// on a `claude --resume` pane the check could match the PREVIOUS turn's prompt
/// still rendered on screen and submit before the new prompt was typed. With
/// the task text last, the tail stays unique per task.
fn compose_prompt(params: &TaskDispatchParams) -> String {
    let task_text = params.task.body.as_ref().unwrap_or(&params.task.title);
    match &params.extra_context {
        Some(Value::String(s)) => format!("{s}\n\n---\n{task_text}"),
        Some(other) => format!("{other}\n\n---\n{task_text}"),
        None => task_text.clone(),
    }
}

/// Treat a "missing pane" error as success (for idempotent teardown).
fn ignore_missing(result: Result<Value, HerdrError>) -> Result<(), HerdrError> {
    match result {
        Ok(_) => Ok(()),
        Err(e) if e.is_missing() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_is_read_off_the_pane_id() {
        assert_eq!(workspace_of("w1:p2"), Some("w1"));
        assert_eq!(workspace_of("w1T:p10"), Some("w1T"));
        // A hand-written session id without the herdr shape names no workspace
        // (the pane still closes; only the workspace sweep is skipped).
        assert_eq!(workspace_of("bare-pane"), None);
    }

    #[test]
    fn agent_started_covers_every_active_status() {
        assert!(agent_started("working"));
        assert!(agent_started("blocked"));
        // Fast answers can be `done` before the first status poll.
        assert!(agent_started("done"));
        // Not yet acting on the prompt: Enter must be pressed (again).
        assert!(!agent_started("idle"));
        assert!(!agent_started("unknown"));
    }

    #[test]
    fn prompt_marker_is_the_squashed_tail() {
        // The tail identifies the prompt: the input box scrolls to the cursor,
        // so a long prompt's head is off-screen.
        let marker = prompt_marker("Reply to this\n\nthread context …\nlast line of the prompt");
        assert!(marker.len() <= "lastlineoftheprompt".len() + 8);
        assert!(marker.ends_with("lastlineoftheprompt"));
        assert!(!marker.contains(' '));

        // A prompt shorter than the marker window is used whole.
        assert_eq!(prompt_marker("hi there"), "hithere");
    }

    #[test]
    fn prompt_marker_handles_multibyte_tails() {
        // Slicing must land on a char boundary, not mid-codepoint.
        let marker = prompt_marker("質問です\n\nzsh の設定はどこにありますか？教えてください。");
        assert!(marker.ends_with("教えてください。"));
    }

    fn dispatch_params(title: &str, body: Option<&str>) -> TaskDispatchParams {
        TaskDispatchParams {
            task: plugin_protocol::task::Task {
                id: "C1:1.0".into(),
                source: "slack".into(),
                title: title.into(),
                body: body.map(str::to_string),
                repo_hint: None,
                labels: vec![],
                priority: 0,
                status: None,
                url: None,
                assignee: None,
                thread_key: None,
            },
            worktree_path: "/wt".into(),
            mode: plugin_protocol::methods::ExecutionMode::Plan,
            extra_context: None,
            job_id: None,
            resume_session_id: None,
            hook: None,
        }
    }

    #[test]
    fn compose_prompt_skips_the_truncated_title_when_a_body_exists() {
        // Sources truncate the title to a snippet; the body carries the full
        // text. Typing both showed a cut-off duplicate first line in the pane.
        let params = dispatch_params(
            "Slack: tomoya in #dev: エイリアスはどのフ",
            Some("full task body"),
        );
        assert_eq!(compose_prompt(&params), "full task body");

        // Title-only tasks still get a prompt.
        let params = dispatch_params("bare title", None);
        assert_eq!(compose_prompt(&params), "bare title");
    }

    #[test]
    fn compose_prompt_puts_string_extra_context_first_as_raw_text() {
        // A string extra_context (e.g. core's marker self-report instruction) is
        // a PREAMBLE: the task text must stay last so the prompt's tail — what
        // submit_prompt matches on screen — stays unique per task (a constant
        // suffix would false-match the previous turn's prompt on a resume pane).
        // Raw text: quotes inside the instruction (e.g. reason="...") must come
        // through unescaped, with no JSON wrapping around the whole string.
        let mut params = dispatch_params("t", Some("unique task body"));
        params.extra_context = Some(Value::String(
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked".into(),
        ));
        let prompt = compose_prompt(&params);
        assert_eq!(
            prompt,
            "end with <<STATUS:NEEDS_INPUT reason=\"...\">> when blocked\n\n---\nunique task body"
        );
        assert!(
            prompt.ends_with("unique task body"),
            "the task text is the tail"
        );
        assert!(
            !prompt.contains("\\\""),
            "quotes are not JSON-escaped: {prompt}"
        );

        // Non-string values keep their JSON rendering (still as preamble).
        params.extra_context = Some(serde_json::json!({"base": "main"}));
        assert!(compose_prompt(&params).starts_with("{\"base\":\"main\"}\n\n---\n"));
    }
}
