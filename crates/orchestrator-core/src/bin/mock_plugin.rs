//! Minimal mock plugin used by the plugin-host and run-loop integration tests.
//!
//! Speaks JSON-RPC 2.0 over NDJSON on stdio (F-51). It is intentionally tiny —
//! the full mock plugin suite lands in #66. Behaviour is driven by the
//! `initialize` config so one binary can play every plugin kind:
//!
//! - `initialize` → stores the config; replies with a version and capabilities.
//! - `config/validate` → valid unless the config contains `"invalid": true`.
//! - `tasks/fetch` → returns the config's `"tasks"` array (default: empty).
//! - `task/update_status` / `result/publish` → acknowledge (recorded to the
//!   config's `"notify_log"` file, if set, as `{"method": ..., "params": ...}`).
//! - `task/dispatch` → replies with a fixed `session_id` (`sess-mock`).
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
                Response::result(
                    request_id(&id),
                    serde_json::to_value(InitializeResult {
                        plugin_version: semver::Version::new(0, 1, 0),
                        capabilities: Capabilities {
                            plan_mode: true,
                            state_stream: true,
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
            "task/dispatch" => Response::result(
                request_id(&id),
                serde_json::to_value(TaskDispatchResult {
                    session_id: "sess-mock".to_string(),
                })
                .unwrap(),
            ),
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
    let Some(path) = config.get("notify_log").and_then(Value::as_str) else {
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

/// Convert a JSON id value into a `RequestId` (numbers used by the host).
fn request_id(id: &Value) -> plugin_protocol::RequestId {
    match id.as_i64() {
        Some(n) => plugin_protocol::RequestId::Number(n),
        None => plugin_protocol::RequestId::Str(id.as_str().unwrap_or("").to_string()),
    }
}
