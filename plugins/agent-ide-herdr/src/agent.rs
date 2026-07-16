//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.
//!
//! Protocol facts this adapter is written against (herdr 0.7.4, verified live
//! against real Claude Code in #124, mirrored in
//! `docs/references/herdr-socket-api.md`):
//! - the agent CLI is launched with `agent.start {name, argv, cwd, workspace_id}`
//!   (`workspace.create` has no command params)
//! - the prompt **cannot** ride in `argv`: a multi-line prompt passed that way
//!   is never submitted, and every task body here is multi-line. It is typed in
//!   with `agent.send` and submitted with Enter — both confirmed, never
//!   fire-and-forget, because the CLI accepts keystrokes before it acts on them
//!   (see [`HerdrAgent::submit_prompt`])
//! - events arrive as `{event, data}` envelopes whose kind separator is
//!   inconsistent (`pane.agent_status_changed` but `pane_exited`), so kinds are
//!   compared normalized; `pane_exited` carries **no exit code**
//! - completion is reported as either `done` or a `working → idle` transition
//!   (which one depends on how herdr detects the agent), so both are honored —
//!   the idle path is debounced against screen-manifest flicker
//! - the pane is **not** a usable answer artifact (`pane.read` returns a copy of
//!   the screen: no scrollback, TUI chrome, long replies lose their head), so
//!   the terminal `done` notification carries the answer read from the agent's
//!   own transcript ([`crate::transcript`]), falling back to the screen only
//!   when no transcript is available

use std::time::Duration;

use plugin_protocol::methods::{
    AgentState, ExecutionMode, SessionAttachResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, extract_answer, extract_question, map_agent_status, squash_ws};
use crate::transcript::{self, AgentSession};
use crate::transport::{HerdrTransport, SUBSCRIPTION_CLOSED_EVENT};

/// How long a `working → idle` transition must hold before it is finalized as
/// `done`. Screen-manifest agent status can flicker; re-reading the pane after
/// this delay filters transient idles (#124).
const IDLE_CONFIRM_DELAY: Duration = Duration::from_secs(2);

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
        let (program, args) = self.config.launch_command(plan);

        // A workspace per task keeps agent panes out of the operator's own
        // workspaces (there is no "start session" method; the workspace is the
        // container).
        let created = self
            .client
            .call(
                "workspace.create",
                json!({
                    "cwd": params.worktree_path,
                    "label": format!("totsuka {}", params.task.id),
                }),
            )
            .await?;
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
            .start_agent(&params, workspace_id, program, args)
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
    ) -> Result<TaskDispatchResult, HerdrError> {
        let argv: Vec<String> = std::iter::once(program).chain(args).collect();
        let started = self
            .client
            .call(
                "agent.start",
                json!({
                    "name": format!("totsuka {}", params.task.id),
                    "argv": argv,
                    "cwd": params.worktree_path,
                    "workspace_id": workspace_id,
                    "focus": false,
                }),
            )
            .await?;
        let agent = started.get("agent").unwrap_or(&started);
        let pane_id = agent
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| HerdrError::InvalidResponse("agent.start returned no pane_id".into()))?
            .to_string();

        self.submit_prompt(&pane_id, &compose_prompt(params))
            .await?;

        // The agent's own session id (for `claude --resume`, and for finding its
        // transcript) is reported by its herdr integration hook during startup;
        // by now it is normally there, and an empty one only degrades resume.
        let agent_session_id = pane_record(&self.client, &pane_id)
            .await
            .ok()
            .and_then(|pane| AgentSession::from_pane(&pane))
            .map(|session| session.value)
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

    /// Start streaming state changes for a session (F-38): subscribe to herdr
    /// pane events, then spawn a task that maps each event to a
    /// [`StateNotification`] on the returned channel. The stream ends after a
    /// terminal state (`done`/`failed`).
    pub async fn start_state_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane_id = handle.pane_id.clone();
        let subscriptions = json!([
            { "type": "pane.agent_status_changed", "pane_id": pane_id },
            { "type": "pane.exited", "pane_id": pane_id },
        ]);
        // Take the event receiver *before* subscribing, so events herdr pushes
        // immediately after the ACK are buffered rather than raced past.
        let mut events = self.client.events();
        self.client.subscribe_events(subscriptions).await?;

        let client = self.client.clone();
        let session_id = session_id.to_string();
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            // Seed from the pane's status now: `dispatch` returns only once the
            // agent started, so whatever it reports here is real progress —
            // including an answer that already finished, whose transition would
            // otherwise have been missed between dispatch and this subscribe.
            let mut previous = match pane_status(&client, &pane_id).await {
                Ok(status) => map_agent_status(&status, AgentState::Running),
                Err(_) => AgentState::Running,
            };
            if matches!(previous, AgentState::Done | AgentState::Idle) {
                let log_chunk = fetch_answer(&client, &pane_id).await;
                let _ = tx.send(StateNotification {
                    session_id: session_id.clone(),
                    state: AgentState::Done,
                    log_chunk,
                });
                return;
            }
            if previous == AgentState::WaitingInput {
                let log_chunk = fetch_question(&client, &pane_id).await;
                let _ = tx.send(StateNotification {
                    session_id: session_id.clone(),
                    state: AgentState::WaitingInput,
                    log_chunk,
                });
            }

            loop {
                let candidate = match events.recv().await {
                    Ok(event) => match classify_event(&event, &pane_id, previous) {
                        Some(c) => c,
                        None => continue,
                    },
                    // Lagged past the buffer: a dropped batch might have held
                    // the terminal event, so re-derive current state from herdr
                    // rather than risk blocking forever on the next event.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::warn!(dropped, "herdr event stream lagged; resyncing state");
                        match resync_state(&client, &pane_id, previous).await {
                            Some(s) => s,
                            None => continue,
                        }
                    }
                    // The transport closed: end the stream.
                    Err(_) => break,
                };
                // Agents that herdr watches by screen manifest report no `done`;
                // for them a `working → idle` transition is the completion
                // signal. Confirm it still holds after a short delay so screen
                // flicker cannot fake a completion.
                let state = if previous == AgentState::Running && candidate == AgentState::Idle {
                    tokio::time::sleep(IDLE_CONFIRM_DELAY).await;
                    match pane_status(&client, &pane_id).await {
                        Ok(status) if status == "idle" || status == "done" => AgentState::Done,
                        Ok(status) => map_agent_status(&status, previous),
                        // The pane vanished between idle and the re-check: the
                        // agent ended without a confirmed completion.
                        Err(e) if e.is_missing() => AgentState::Failed,
                        Err(_) => continue,
                    }
                } else {
                    candidate
                };
                // A repeated state is deduped here, so a *new* question in a
                // second consecutive block is not re-delivered (best-effort,
                // F-35).
                if state == previous {
                    continue;
                }
                previous = state;

                let log_chunk = match state {
                    // On a block, best-effort attach the question (F-35).
                    AgentState::WaitingInput => fetch_question(&client, &pane_id).await,
                    // On completion, carry the agent's answer — this is the only
                    // channel it reaches the Orchestrator's `output = source`
                    // artifact through.
                    AgentState::Done => fetch_answer(&client, &pane_id).await,
                    _ => None,
                };
                let terminal = matches!(state, AgentState::Done | AgentState::Failed);
                if tx
                    .send(StateNotification {
                        session_id: session_id.clone(),
                        state,
                        log_chunk,
                    })
                    .is_err()
                {
                    break; // the consumer dropped
                }
                if terminal {
                    break;
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

/// Re-derive the current state after a dropped-event lag: read the pane's
/// status, or treat a vanished pane as a terminal `failed`. `None` means the
/// state is currently unknowable (transient error) — hold and retry.
async fn resync_state<T: HerdrTransport>(
    client: &T,
    pane_id: &str,
    previous: AgentState,
) -> Option<AgentState> {
    match pane_status(client, pane_id).await {
        Ok(status) => Some(map_agent_status(&status, previous)),
        // The pane is gone → the agent ended; surface a terminal state.
        Err(e) if e.is_missing() => Some(AgentState::Failed),
        Err(_) => None,
    }
}

/// The state a single herdr event envelope implies, or `None` if it is for
/// another pane or carries no state signal we map.
///
/// Events arrive as `{event: "<kind>", data: {pane_id, …}}`. Every envelope —
/// herdr's own and the transport's synthetic close — is filtered by pane first:
/// one broadcast carries every subscription in the process, and herdr replays
/// other panes' history on connect, so an unfiltered event would fail a healthy
/// task.
fn classify_event(event: &Value, pane_id: &str, previous: AgentState) -> Option<AgentState> {
    let kind = event.get("event").and_then(Value::as_str)?;
    let data = event.get("data")?;
    if data.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
        return None;
    }
    // herdr is inconsistent about the separator in event kinds
    // (`pane.agent_status_changed` but `pane_exited`), so compare normalized.
    match kind.replace('.', "_").as_str() {
        "pane_agent_status_changed" => {
            let status = data.get("agent_status").and_then(Value::as_str)?;
            Some(map_agent_status(status, previous))
        }
        // herdr 0.7.x carries no exit code; completion is signalled by status
        // *before* any exit, so an exit that arrives first means the agent
        // ended without completing.
        "pane_exited" => Some(AgentState::Failed),
        // The subscription connection died; deliver a failure rather than hang
        // a stream that will never see another event. (The pane may still be
        // alive — recovery re-attaches, F-37.)
        SUBSCRIPTION_CLOSED_EVENT => Some(AgentState::Failed),
        _ => None,
    }
}

/// Best-effort question text for a blocked agent (F-35), from the visible pane
/// content (herdr has no scrollback field; `pane.read` is the reader).
async fn fetch_question<T: HerdrTransport>(client: &T, pane_id: &str) -> Option<String> {
    let text = read_pane_text(client, pane_id, "visible", SCREEN_LINES).await?;
    extract_question(&text)
}

/// The agent's final answer for the terminal `done` notification.
///
/// Prefers the agent's own transcript — exact and complete. The screen is only
/// a fallback (`detection` is herdr's chrome-free conversation view, but it is
/// still screen-sized, so a long answer loses its head): better a truncated
/// draft the operator can see and fix than none at all.
async fn fetch_answer<T: HerdrTransport>(client: &T, pane_id: &str) -> Option<String> {
    if let Ok(pane) = pane_record(client, pane_id).await
        && let Some(session) = AgentSession::from_pane(&pane)
        && let Some(reader) = transcript::for_agent(&session.agent)
    {
        let cwd = pane.get("cwd").and_then(Value::as_str).unwrap_or_default();
        if let Some(answer) = reader.last_answer(&session, std::path::Path::new(cwd)) {
            return Some(answer);
        }
        tracing::warn!(
            pane_id,
            agent = %session.agent,
            "no transcript answer; falling back to the screen (the draft may be truncated)"
        );
    }
    let text = read_pane_text(client, pane_id, "detection", SCREEN_LINES).await?;
    extract_answer(&text)
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

/// Compose the agent prompt from the task (title + body + any extra context).
fn compose_prompt(params: &TaskDispatchParams) -> String {
    let mut prompt = params.task.title.clone();
    if let Some(body) = &params.task.body {
        prompt.push_str("\n\n");
        prompt.push_str(body);
    }
    if let Some(extra) = &params.extra_context {
        prompt.push_str("\n\n---\n");
        prompt.push_str(&extra.to_string());
    }
    prompt
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
}
