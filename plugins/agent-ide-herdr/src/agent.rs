//! The herdr adapter logic (F-30〜F-38): translate the Orchestrator's
//! agent_ide calls into herdr Socket API method calls and event streams.

use plugin_protocol::methods::{
    AgentState, ExecutionMode, SessionAttachResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::state::{SessionHandle, extract_question, map_agent_status, state_from_exit};
use crate::transport::HerdrTransport;

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

    /// Dispatch a task (F-31/F-37): create a herdr pane in the worktree running
    /// the agent CLI (in plan mode when asked, F-36), send the task prompt, and
    /// return a `(pane_id, agent_session_id)` re-attach handle as the session id.
    pub async fn dispatch(
        &self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, HerdrError> {
        let plan = params.mode == ExecutionMode::Plan;
        let (program, args) = self.config.launch_command(plan);

        // herdr has no "start session" — create a pane whose cwd is the worktree
        // and whose command is the agent CLI.
        let created = self
            .client
            .call(
                "workspace.create",
                json!({ "cwd": params.worktree_path, "command": program, "args": args }),
            )
            .await?;
        let pane_id = created
            .get("pane_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HerdrError::InvalidResponse("workspace.create returned no pane_id".into())
            })?
            .to_string();
        // The agent's native session id (for `claude --resume`) may be reported
        // by herdr at creation; empty until detected (resume then degrades).
        let agent_session_id = created
            .get("agent_session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let prompt = compose_prompt(&params);
        self.client
            .call(
                "agent.send",
                json!({ "pane_id": pane_id, "message": prompt }),
            )
            .await?;

        let handle = SessionHandle::new(pane_id, agent_session_id);
        Ok(TaskDispatchResult {
            session_id: handle.encode(),
        })
    }

    /// Re-attach to a dispatched session (F-37): reconnect state via
    /// `session.snapshot`, confirm the pane is alive with `pane.get`, and report
    /// its current mapped state. A vanished pane (`not_found`) is reported as
    /// `attached: false` (the Orchestrator's recovery then defers to a human).
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, HerdrError> {
        let handle = SessionHandle::decode(session_id);
        // Refresh state after a possible reconnect; a missing session is a
        // clean "not attached", any other snapshot error is a real failure.
        if let Err(e) = self.client.call("session.snapshot", json!({})).await
            && !e.is_missing()
        {
            return Err(e);
        }
        match self
            .client
            .call("pane.get", json!({ "pane_id": handle.pane_id }))
            .await
        {
            Ok(pane) => {
                let status = pane
                    .get("agent_status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Ok(SessionAttachResult {
                    attached: true,
                    state: map_agent_status(status, AgentState::Idle),
                })
            }
            // The pane is gone → not attached (F-37); the Orchestrator (#57)
            // routes this to human confirmation rather than auto-failing.
            Err(e) if e.is_missing() => Ok(SessionAttachResult {
                attached: false,
                state: AgentState::Failed,
            }),
            Err(e) => Err(e),
        }
    }

    /// Cancel a task: interrupt the agent (`ctrl+c`) then close its pane. Both
    /// steps tolerate an already-gone pane, so cancel is idempotent.
    pub async fn cancel(&self, session_id: &str) -> Result<(), HerdrError> {
        let handle = SessionHandle::decode(session_id);
        let pane = json!({ "pane_id": handle.pane_id });
        ignore_missing(
            self.client
                .call(
                    "pane.send_keys",
                    json!({ "pane_id": handle.pane_id, "keys": ["ctrl+c"] }),
                )
                .await,
        )?;
        ignore_missing(self.client.call("pane.close", pane).await)?;
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
                let event = match events.recv().await {
                    Ok(ev) => ev,
                    // Lagged past the buffer: keep going with the next event.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    // The transport closed: end the stream.
                    Err(_) => break,
                };
                if event.get("pane_id").and_then(Value::as_str) != Some(pane_id.as_str()) {
                    continue; // an event for a different pane on the shared socket
                }
                let Some(state) = next_state(&event, previous) else {
                    continue;
                };
                if state == previous {
                    continue; // no transition to report
                }
                previous = state;

                // On a block, best-effort attach the question from scrollback.
                let log_chunk = if state == AgentState::WaitingInput {
                    fetch_question(&client, &pane_id).await
                } else {
                    None
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

/// The state a single herdr event implies, or `None` if it carries no state
/// signal we map.
fn next_state(event: &Value, previous: AgentState) -> Option<AgentState> {
    match event.get("type").and_then(Value::as_str)? {
        "pane.agent_status_changed" => {
            let status = event.get("agent_status").and_then(Value::as_str)?;
            Some(map_agent_status(status, previous))
        }
        "pane.exited" => {
            let code = event.get("exit_code").and_then(Value::as_i64).unwrap_or(0);
            Some(state_from_exit(code))
        }
        _ => None,
    }
}

/// Best-effort question text for a blocked agent (F-35), from pane scrollback.
async fn fetch_question<T: HerdrTransport>(client: &T, pane_id: &str) -> Option<String> {
    let pane = client
        .call("pane.get", json!({ "pane_id": pane_id }))
        .await
        .ok()?;
    let scrollback = pane.get("scrollback").and_then(Value::as_str)?;
    extract_question(scrollback)
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
