//! Minimal mock plugin used by the plugin-host integration test.
//!
//! Speaks JSON-RPC 2.0 over NDJSON on stdio (F-51). It is intentionally tiny —
//! the full mock plugin suite lands in #66. Behaviour:
//!
//! - `initialize` → replies with a version and capabilities.
//! - `config/validate` → valid unless the config contains `"invalid": true`.
//! - `crash` → exits immediately with code 1 (to test crash isolation).
//! - `shutdown` → replies, then exits 0.
//! - anything else → method-not-found error.

use std::io::{BufRead, Write};

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code};
use plugin_protocol::methods::{ConfigValidateResult, InitializeResult};
use plugin_protocol::{Capabilities, manifest::OutputCapability};
use serde_json::Value;

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // A message without an `id` is a notification; per JSON-RPC it must not
        // be answered.
        if request.get("id").is_none() {
            continue;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match method {
            "initialize" => Response::result(
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
            ),
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
            "state/subscribe" => {
                // Emit one notification (no id), then acknowledge (F-38).
                let note = Notification::new(
                    "state/notification",
                    Some(serde_json::json!({
                        "session_id": "sess-mock",
                        "state": "running",
                        "log_chunk": "compiling..."
                    })),
                );
                let _ = writeln!(stdout, "{}", serde_json::to_string(&note).unwrap());
                let _ = stdout.flush();
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

/// Convert a JSON id value into a `RequestId` (numbers used by the host).
fn request_id(id: &Value) -> plugin_protocol::RequestId {
    match id.as_i64() {
        Some(n) => plugin_protocol::RequestId::Number(n),
        None => plugin_protocol::RequestId::Str(id.as_str().unwrap_or("").to_string()),
    }
}
