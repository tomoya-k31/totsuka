//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.
//!
//! Protocol facts this adapter is written against (herdr 0.7.4, #124, mirrored
//! in `docs/references/herdr-socket-api.md`):
//! - the agent CLI is launched with `agent.start {name, argv, cwd, workspace_id}`
//!   (`workspace.create` has no command params); the task prompt travels in
//!   `argv` so no separate send/Enter round-trip can race the CLI's startup
//! - events arrive as `{event: "pane_agent_status_changed"|"pane_exited",
//!   data: {...}}` envelopes; `pane_exited` carries **no exit code**
//! - screen-manifest agents (Claude Code) never report `done`, so a
//!   `working → idle` transition is the completion signal — confirmed with a
//!   short re-check before it is finalized (`IDLE_CONFIRM_DELAY`)
//! - the agent's final output is not pushed anywhere; it is read from the
//!   still-open pane with `pane.read` and attached to the terminal `done`
//!   notification as its `log_chunk` (the Orchestrator accumulates log chunks
//!   into the `output = source` publish artifact)

use std::time::Duration;

use plugin_protocol::methods::{
    AgentState, ExecutionMode, SessionAttachResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, extract_question, map_agent_status};
use crate::transport::{HerdrTransport, SUBSCRIPTION_CLOSED_EVENT};

/// How long a `working → idle` transition must hold before it is finalized as
/// `done`. Screen-manifest agent status can flicker; re-reading the pane after
/// this delay filters transient idles (#124).
const IDLE_CONFIRM_DELAY: Duration = Duration::from_secs(2);

/// How many trailing lines of pane output the terminal `done` notification
/// carries (the `output = source` publish artifact).
const FINAL_OUTPUT_LINES: u64 = 400;

/// How many visible lines are scanned for a blocked agent's question (F-35).
const QUESTION_LINES: u64 = 60;

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
    /// start the agent CLI in it (plan mode when asked, F-36) with the task
    /// prompt as the trailing argv element, and return a
    /// `(pane_id, agent_session_id)` re-attach handle as the session id.
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
                json!({ "cwd": params.worktree_path, "label": format!("totsuka {}", params.task.id) }),
            )
            .await?;
        let workspace_id = created
            .get("workspace")
            .and_then(|w| w.get("workspace_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HerdrError::InvalidResponse("workspace.create returned no workspace_id".into())
            })?;

        // The prompt rides in argv (`claude … "<prompt>"`): `agent.send` writes
        // literal text without submitting it, and racing a send against the
        // CLI's startup is exactly the kind of flake argv avoids.
        let mut argv = vec![program];
        argv.extend(args);
        argv.push(compose_prompt(&params));
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
        // The agent's native session id (for `claude --resume`) is reported by
        // the integration hook after startup; empty until detected (resume
        // then degrades).
        let agent_session_id = agent
            .get("agent_session")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let handle = SessionHandle::new(pane_id, agent_session_id);
        Ok(TaskDispatchResult {
            session_id: handle.encode(),
        })
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

    /// Cancel a task: interrupt the agent (`ctrl+c`) then close its pane. The
    /// interrupt is best-effort — even if it fails the pane is still closed —
    /// and an already-gone pane is treated as success, so cancel is idempotent.
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
            let mut previous = AgentState::Idle;
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
                // A `working → idle` transition is the completion signal for
                // agents that never report `done` (Claude Code); confirm it
                // still holds after a short delay to filter screen-manifest
                // flicker, then finalize as `done`.
                let state = if previous == AgentState::Running && candidate == AgentState::Idle {
                    tokio::time::sleep(IDLE_CONFIRM_DELAY).await;
                    match pane_status(&client, &pane_id).await {
                        Ok(status) if status == "idle" => AgentState::Done,
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
                    // On completion, carry the agent's final output — the pane
                    // is still open (idle-confirmed completion), and this is
                    // the only channel the answer reaches the Orchestrator's
                    // `output = source` artifact through.
                    AgentState::Done => fetch_final_output(&client, &pane_id).await,
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
}

/// The pane's current `agent_status` (from the `pane.get` record, which nests
/// the pane under `result.pane`).
async fn pane_status<T: HerdrTransport>(client: &T, pane_id: &str) -> Result<String, HerdrError> {
    let result = client
        .call("pane.get", json!({ "pane_id": pane_id }))
        .await?;
    let pane = result.get("pane").unwrap_or(&result);
    Ok(pane
        .get("agent_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string())
}

/// Re-derive the current state after a dropped-event lag or a closed
/// subscription: read the pane's status, or treat a vanished pane as a terminal
/// `failed`. `None` means the state is currently unknowable (transient error)
/// — hold and retry.
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
/// Events arrive as `{event: "<kind>", data: {pane_id, …}}` with
/// underscore kinds; a subscription that herdr replays on connect can include
/// other panes' history, so the pane filter here is load-bearing.
fn classify_event(event: &Value, pane_id: &str, previous: AgentState) -> Option<AgentState> {
    let kind = event.get("event").and_then(Value::as_str)?;
    if kind == SUBSCRIPTION_CLOSED_EVENT {
        // The subscription connection died; deliver a failure rather than
        // hang a stream that will never see another event. (The pane may
        // still be alive — recovery re-attaches, F-37.)
        return Some(AgentState::Failed);
    }
    let data = event.get("data")?;
    if data.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
        return None;
    }
    match kind {
        "pane_agent_status_changed" => {
            let status = data.get("agent_status").and_then(Value::as_str)?;
            Some(map_agent_status(status, previous))
        }
        // herdr 0.7.x carries no exit code; completion is signalled by the
        // idle-confirm path *before* any exit, so an exit that arrives first
        // means the agent ended without completing.
        "pane_exited" => Some(AgentState::Failed),
        _ => None,
    }
}

/// Best-effort question text for a blocked agent (F-35), from the visible
/// pane content (herdr has no scrollback field; `pane.read` is the reader).
async fn fetch_question<T: HerdrTransport>(client: &T, pane_id: &str) -> Option<String> {
    let text = read_pane_text(client, pane_id, "visible", QUESTION_LINES).await?;
    extract_question(&text)
}

/// The agent's final output for the terminal `done` notification, read from
/// the still-open pane.
async fn fetch_final_output<T: HerdrTransport>(client: &T, pane_id: &str) -> Option<String> {
    let text = read_pane_text(client, pane_id, "recent", FINAL_OUTPUT_LINES).await?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
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
