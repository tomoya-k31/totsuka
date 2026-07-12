//! The orca adapter logic (F-30〜F-38): translate the Orchestrator's agent_ide
//! calls into `orca` CLI invocations and a polled state stream.

use std::time::{Duration, Instant};

use plugin_protocol::methods::{
    AgentState, ExecutionMode, SessionAttachResult, StateNotification, TaskDispatchParams,
    TaskDispatchResult,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::cli::OrcaCli;
use crate::config::{OrcaConfig, worktree_name};
use crate::error::OrcaError;
use crate::state::{extract_question, map_orca_state};

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

    /// Dispatch a task (F-31): create an orca worktree that launches the agent
    /// with the task prompt (plan intent prepended in plan mode, F-36). Returns
    /// the orca worktree id as the session id (orca keeps the session history,
    /// so a bare id suffices for re-attach — "weak absorption", F-37).
    pub async fn dispatch(
        &self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, OrcaError> {
        let plan = params.mode == ExecutionMode::Plan;
        let repo = self.config.repo_selector_for(&params.worktree_path);
        let name = worktree_name(&params.task.id);
        let prompt = self.config.compose_prompt(&compose_prompt(&params), plan);

        let args = vec![
            "worktree".into(),
            "create".into(),
            "--repo".into(),
            repo,
            "--name".into(),
            name,
            "--agent".into(),
            self.config.agent.clone(),
            "--prompt".into(),
            prompt,
            "--setup".into(),
            self.config.setup.clone(),
            "--json".into(),
        ];
        let created = self.cli.run(args).await?;
        let worktree_id = worktree_id(&created).ok_or_else(|| {
            OrcaError::InvalidResponse("worktree create returned no worktree id".into())
        })?;
        Ok(TaskDispatchResult {
            session_id: worktree_id,
        })
    }

    /// Re-attach to a dispatched session (F-37): confirm the worktree still
    /// exists via `worktree show` and report its current mapped state. A missing
    /// worktree is `attached: false` (the Orchestrator's recovery defers to a
    /// human). Claude's `claude --resume` is handled by orca itself.
    pub async fn attach(&self, session_id: &str) -> Result<SessionAttachResult, OrcaError> {
        let args = vec![
            "worktree".into(),
            "show".into(),
            "--worktree".into(),
            format!("id:{session_id}"),
            "--json".into(),
        ];
        match self.cli.run(args).await {
            Ok(show) => {
                let state = entry_state(&show).unwrap_or("unknown");
                Ok(SessionAttachResult {
                    attached: true,
                    state: map_orca_state(state, AgentState::Idle),
                })
            }
            Err(e) if e.is_missing() => Ok(SessionAttachResult {
                attached: false,
                state: AgentState::Failed,
            }),
            Err(e) => Err(e),
        }
    }

    /// Cancel a task: remove the worktree (which stops the agent). An
    /// already-removed worktree is treated as success, so cancel is idempotent.
    pub async fn cancel(&self, session_id: &str) -> Result<(), OrcaError> {
        let args = vec![
            "worktree".into(),
            "rm".into(),
            "--worktree".into(),
            format!("id:{session_id}"),
            "--force".into(),
            "--json".into(),
        ];
        match self.cli.run(args).await {
            Ok(_) => Ok(()),
            Err(e) if e.is_missing() => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Start streaming state changes for a session (F-38): poll
    /// `orca worktree ps` for the worktree's state dot, mapping each change to a
    /// [`StateNotification`] on the returned channel. Pacing uses
    /// `orca terminal wait --for tui-idle` when a terminal handle is known, else
    /// a fixed poll interval. The stream ends after a terminal state.
    pub async fn start_state_stream(
        &self,
        session_id: &str,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, OrcaError> {
        let cli = self.cli.clone();
        let session_id = session_id.to_string();
        let poll = Duration::from_millis(self.config.poll_interval_ms);
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut previous = AgentState::Idle;
            let mut consecutive_errors = 0u32;
            loop {
                // Resolve the current state. A worktree merely absent from `ps`
                // is confirmed via `worktree show` before concluding the run
                // ended — a transient/empty `ps` must not be read as `done`.
                let resolved = match cli.run(ps_args()).await {
                    Ok(ps) => match find_worktree(&ps, &session_id) {
                        Some(entry) => Resolved::Alive(
                            map_orca_state(entry_state(&entry).unwrap_or("unknown"), previous),
                            entry_terminal(&entry).map(str::to_string),
                        ),
                        None => confirm(&cli, &session_id, previous).await,
                    },
                    Err(e) if e.is_missing() => confirm(&cli, &session_id, previous).await,
                    // A transient CLI error: don't guess a state, retry.
                    Err(_) => Resolved::Unknown,
                };

                let (state, terminal_handle) = match resolved {
                    Resolved::Alive(state, terminal) => {
                        consecutive_errors = 0;
                        (state, terminal)
                    }
                    // Confirmed gone → the run genuinely ended.
                    Resolved::Gone => {
                        consecutive_errors = 0;
                        (AgentState::Done, None)
                    }
                    // Couldn't read state; retry, but after repeated failures
                    // surface a terminal `failed` rather than hang or die silently.
                    Resolved::Unknown => {
                        consecutive_errors += 1;
                        if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                            let _ = tx.send(StateNotification {
                                session_id: session_id.clone(),
                                state: AgentState::Failed,
                                log_chunk: Some(
                                    "orca state became unreadable after repeated attempts".into(),
                                ),
                            });
                            break;
                        }
                        pace(&cli, None, poll).await;
                        continue;
                    }
                };

                if state != previous {
                    previous = state;
                    let log_chunk = if state == AgentState::WaitingInput {
                        fetch_question(&cli, terminal_handle.as_deref()).await
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

                // Pace the next poll: block on tui-idle when we have a terminal
                // handle (returns promptly on idle), else sleep.
                pace(&cli, terminal_handle.as_deref(), poll).await;
            }
        });

        Ok(rx)
    }
}

/// The number of consecutive unreadable-state polls after which the stream
/// gives up and reports `failed`, rather than looping or dying silently.
const MAX_CONSECUTIVE_ERRORS: u32 = 5;

/// The state resolution for one poll: a live worktree with a state, a confirmed
/// gone worktree (run over), or an unreadable state (retry).
enum Resolved {
    Alive(AgentState, Option<String>),
    Gone,
    Unknown,
}

/// Confirm whether a worktree missing from `ps` is truly gone, via
/// `worktree show`: a not-found error means gone; a successful read means it is
/// still alive (the `ps` miss was transient); any other error is unknown.
async fn confirm<C: OrcaCli>(cli: &C, session_id: &str, previous: AgentState) -> Resolved {
    let args = vec![
        "worktree".into(),
        "show".into(),
        "--worktree".into(),
        format!("id:{session_id}"),
        "--json".into(),
    ];
    match cli.run(args).await {
        Ok(show) => Resolved::Alive(
            map_orca_state(entry_state(&show).unwrap_or("unknown"), previous),
            entry_terminal(&show).map(str::to_string),
        ),
        Err(e) if e.is_missing() => Resolved::Gone,
        Err(_) => Resolved::Unknown,
    }
}

/// `orca worktree ps --json` argument vector.
fn ps_args() -> Vec<String> {
    vec!["worktree".into(), "ps".into(), "--json".into()]
}

/// A fixed minimum spacing between state polls. Independent of `poll_interval`
/// (which may be tiny in tests / misconfiguration) so a `terminal wait` that
/// returns instantly can never spin the loop. Kept small so `done` is still
/// detected promptly once the TUI goes idle.
const MIN_POLL_FLOOR: Duration = Duration::from_millis(250);

/// Wait before the next state poll: `orca terminal wait --for tui-idle` when a
/// terminal handle is known (best-effort; errors are ignored), else sleep.
///
/// [`MIN_POLL_FLOOR`] is always honored so that a `terminal wait` which returns
/// instantly — because the TUI is already idle (a persistent `waiting_input`)
/// or because that flag errors on this orca build — cannot spin the poll loop
/// into hammering the CLI. During `working`, `terminal wait` blocks up to
/// `poll` on its own, so the floor does not slow normal pacing.
async fn pace<C: OrcaCli>(cli: &C, terminal: Option<&str>, poll: Duration) {
    match terminal {
        Some(handle) => {
            let args = vec![
                "terminal".into(),
                "wait".into(),
                "--terminal".into(),
                handle.to_string(),
                "--for".into(),
                "tui-idle".into(),
                "--timeout-ms".into(),
                poll.as_millis().to_string(),
                "--json".into(),
            ];
            let started = Instant::now();
            let _ = cli.run(args).await; // best-effort pacing
            let elapsed = started.elapsed();
            if elapsed < MIN_POLL_FLOOR {
                tokio::time::sleep(MIN_POLL_FLOOR - elapsed).await;
            }
        }
        None => tokio::time::sleep(poll.max(MIN_POLL_FLOOR)).await,
    }
}

/// Best-effort question text for a blocked agent (F-35) via `terminal read`.
async fn fetch_question<C: OrcaCli>(cli: &C, terminal: Option<&str>) -> Option<String> {
    let handle = terminal?;
    let args = vec![
        "terminal".into(),
        "read".into(),
        "--terminal".into(),
        handle.to_string(),
        "--limit".into(),
        "1000".into(),
        "--json".into(),
    ];
    let read = cli.run(args).await.ok()?;
    let text = read
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            read.get("lines").and_then(Value::as_array).map(|lines| {
                lines
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })?;
    extract_question(&text)
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

/// Extract the worktree id from a `worktree create` response, tolerating a few
/// shapes (`id`, `worktree_id`, or nested `worktree.id`).
fn worktree_id(created: &Value) -> Option<String> {
    created
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| created.get("worktree_id").and_then(Value::as_str))
        .or_else(|| {
            created
                .get("worktree")
                .and_then(|w| w.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

/// Find a worktree entry by id in a `worktree ps` response (`{worktrees: [...]}`
/// or a bare array).
fn find_worktree(ps: &Value, id: &str) -> Option<Value> {
    let list = ps
        .get("worktrees")
        .and_then(Value::as_array)
        .or_else(|| ps.as_array())?;
    list.iter()
        .find(|w| w.get("id").and_then(Value::as_str) == Some(id))
        .cloned()
}

/// The `state` field of a worktree entry / show response.
fn entry_state(entry: &Value) -> Option<&str> {
    entry.get("state").and_then(Value::as_str).or_else(|| {
        entry
            .get("worktree")
            .and_then(|w| w.get("state"))
            .and_then(Value::as_str)
    })
}

/// The terminal handle of a worktree entry, if present.
fn entry_terminal(entry: &Value) -> Option<&str> {
    entry
        .get("terminal")
        .and_then(Value::as_str)
        .or_else(|| entry.get("terminal_handle").and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_worktree_id_from_shapes() {
        assert_eq!(worktree_id(&json!({ "id": "wt1" })).as_deref(), Some("wt1"));
        assert_eq!(
            worktree_id(&json!({ "worktree": { "id": "wt2" } })).as_deref(),
            Some("wt2")
        );
        assert_eq!(worktree_id(&json!({ "nope": true })), None);
    }

    #[test]
    fn finds_worktree_in_list_and_bare_array() {
        let wrapped = json!({ "worktrees": [{ "id": "a", "state": "working" }] });
        assert_eq!(
            entry_state(&find_worktree(&wrapped, "a").unwrap()),
            Some("working")
        );
        let bare = json!([{ "id": "b", "state": "done", "terminal": "t1" }]);
        let entry = find_worktree(&bare, "b").unwrap();
        assert_eq!(entry_terminal(&entry), Some("t1"));
        assert!(find_worktree(&bare, "missing").is_none());
    }
}
