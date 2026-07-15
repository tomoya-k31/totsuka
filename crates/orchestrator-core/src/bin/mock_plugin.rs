//! Minimal mock plugin used by the plugin-host and run-loop integration tests.
//!
//! Speaks JSON-RPC 2.0 over NDJSON on stdio (F-51). It is intentionally tiny —
//! the full mock plugin suite lands in #66. Behaviour is driven by the
//! `initialize` config so one binary can play every plugin kind:
//!
//! - `initialize` → stores the config; replies with a version and capabilities
//!   (`"no_state_stream": true` drops the `state_stream` capability).
//! - `config/validate` → valid unless the config contains `"invalid": true`.
//! - `tasks/fetch` → returns the config's `"tasks"` array (default: empty).
//! - `task/update_status` / `result/publish` → acknowledge (recorded to the
//!   config's `"notify_log"` file, if set, as `{"method": ..., "params": ...}`).
//! - `task/dispatch` → replies with the config's `"session_id"` (default
//!   `sess-mock`); `"commit_on_dispatch": true` leaves a real commit in the
//!   worktree so the pull_request output policy has something to push;
//!   `"crash_on_dispatch": true` exits mid-dispatch (crash isolation, §5.3).
//! - `session/attach` → `attached: false` if the session id contains `gone`,
//!   otherwise `attached: true` with a state chosen from the id (`waiting`,
//!   `done`, `fail`, else `running`) so recovery paths are testable (#57).
//! - `state/subscribe` → emits one `state/notification` per entry of the
//!   config's `"stream_states"` array (default `["running"]`) for the
//!   subscribed session, then acknowledges.
//! - `notify` (notification) → appended to the `"notify_log"` file, if set.
//! - `task/cancel` → acknowledges.
//! - `crash` → exits immediately with code 1 (to test crash isolation).
//! - `shutdown` → replies, then exits 0.
//! - anything else → method-not-found error.

use std::io::{BufRead, Write};

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code};
use plugin_protocol::methods::{
    AgentState, ConfigValidateResult, InitializeResult, SessionAttachResult, TaskDispatchResult,
};
use plugin_protocol::{Capabilities, manifest::OutputCapability};
use serde_json::Value;

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    // The plugin config passed via `initialize` (drives mock behaviour).
    let mut config = Value::Null;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        // A message without an `id` is a notification; per JSON-RPC it must not
        // be answered — but `notify` (F-90) is still observed for tests.
        if request.get("id").is_none() {
            if method == "notify" {
                record(&config, "notify", &params);
            }
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        let response = match method {
            "initialize" => {
                config = params.get("config").cloned().unwrap_or(Value::Null);
                // Recorded to its own file (`init_log`), NOT `notify_log`:
                // tests read notify_log as "observable side effects", and
                // initialize happens even in a dry run.
                record_to(config.get("init_log"), "initialize", &params);
                // `no_state_stream: true` simulates a minimal agent that does
                // not stream state (the orchestrator must refuse to dispatch).
                let state_stream = !config
                    .get("no_state_stream")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Response::result(
                    request_id(&id),
                    serde_json::to_value(InitializeResult {
                        plugin_version: semver::Version::new(0, 1, 0),
                        capabilities: Capabilities {
                            plan_mode: true,
                            state_stream,
                            outputs: vec![OutputCapability::Source],
                            ..Default::default()
                        },
                    })
                    .unwrap(),
                )
            }
            "config/validate" => {
                let invalid = params
                    .get("config")
                    .and_then(|c| c.get("invalid"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Response::result(
                    request_id(&id),
                    serde_json::to_value(ConfigValidateResult {
                        valid: !invalid,
                        errors: if invalid {
                            vec!["config marked invalid → fix it".to_string()]
                        } else {
                            vec![]
                        },
                    })
                    .unwrap(),
                )
            }
            "tasks/fetch" => {
                let tasks = config.get("tasks").cloned().unwrap_or(Value::Array(vec![]));
                Response::result(request_id(&id), serde_json::json!({ "tasks": tasks }))
            }
            "task/update_status" | "result/publish" => {
                record(&config, method, &params);
                Response::result(request_id(&id), Value::Null)
            }
            "task/dispatch" => {
                // `crash_on_dispatch: true` self-destructs mid-dispatch to
                // exercise crash isolation (§5.3) end to end: the host observes
                // EOF and fails the task without the orchestrator dying.
                if config
                    .get("crash_on_dispatch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    std::process::exit(1);
                }
                // Overridable so tests can steer `session/attach` behaviour
                // (ids containing `gone`/`done`/... choose the attach reply).
                let session_id = config
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("sess-mock")
                    .to_string();
                // `commit_on_dispatch: true` makes the mock agent leave a real
                // commit in the worktree, so the pull_request output policy has
                // something to push (the agent's work ends at the commit, F-86).
                if config
                    .get("commit_on_dispatch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && let Some(worktree) = params.get("worktree_path").and_then(Value::as_str)
                {
                    commit_in(worktree);
                }
                Response::result(
                    request_id(&id),
                    serde_json::to_value(TaskDispatchResult { session_id }).unwrap(),
                )
            }
            "session/attach" => {
                let sid = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let attached = !sid.contains("gone");
                let state = if sid.contains("waiting") {
                    AgentState::WaitingInput
                } else if sid.contains("done") {
                    AgentState::Done
                } else if sid.contains("fail") {
                    AgentState::Failed
                } else {
                    AgentState::Running
                };
                Response::result(
                    request_id(&id),
                    serde_json::to_value(SessionAttachResult { attached, state }).unwrap(),
                )
            }
            "task/cancel" => Response::result(request_id(&id), Value::Null),
            "state/subscribe" => {
                // Emit the configured state sequence (default: one `running`)
                // for the subscribed session, then acknowledge (F-38).
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("sess-mock")
                    .to_string();
                let default_states = serde_json::json!(["running"]);
                let states = config
                    .get("stream_states")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| default_states.as_array().unwrap().clone());
                for (i, state) in states.iter().enumerate() {
                    let note = Notification::new(
                        "state/notification",
                        Some(serde_json::json!({
                            "session_id": session_id,
                            "state": state,
                            "log_chunk": if i == 0 { Some("compiling...") } else { None },
                        })),
                    );
                    let _ = writeln!(stdout, "{}", serde_json::to_string(&note).unwrap());
                    let _ = stdout.flush();
                }
                Response::result(request_id(&id), Value::Null)
            }
            "crash" => std::process::exit(1),
            "shutdown" => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(&Response::result(request_id(&id), Value::Null)).unwrap()
                );
                let _ = stdout.flush();
                std::process::exit(0);
            }
            other => Response::error(
                request_id(&id),
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method: {other}"),
                ),
            ),
        };

        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}

/// Append `{"method", "params"}` to the config's `notify_log` file, if set —
/// the observation channel for fire-and-forget calls in integration tests.
fn record(config: &Value, method: &str, params: &Value) {
    record_to(config.get("notify_log"), method, params);
}

/// Append `{"method", "params"}` to the file named by `path`, if set.
fn record_to(path: Option<&Value>, method: &str, params: &Value) {
    let Some(path) = path.and_then(Value::as_str) else {
        return;
    };
    let line = serde_json::json!({ "method": method, "params": params });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Leave an empty commit in `worktree` (mock agent "work"). Signing is
/// disabled and identity is injected so it never blocks in CI. Spawn/exit
/// failures are logged to stderr (forwarded to the orchestrator log) so a
/// misconfigured test worktree fails loudly rather than silently proceeding.
fn commit_in(worktree: &str) {
    match std::process::Command::new("git")
        .current_dir(worktree)
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.email=totsuka@test",
            "-c",
            "user.name=totsuka",
            "commit",
            "--allow-empty",
            "-m",
            "agent work",
        ])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "mock_plugin: commit_on_dispatch failed in {worktree}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("mock_plugin: could not run git commit in {worktree}: {e}"),
    }
}

/// Convert a JSON id value into a `RequestId` (numbers used by the host).
fn request_id(id: &Value) -> plugin_protocol::RequestId {
    match id.as_i64() {
        Some(n) => plugin_protocol::RequestId::Number(n),
        None => plugin_protocol::RequestId::Str(id.as_str().unwrap_or("").to_string()),
    }
}
