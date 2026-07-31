//! Minimal mock plugin used by the plugin-host and run-loop integration tests.
//!
//! Speaks JSON-RPC 2.0 over NDJSON on stdio (F-51). It is intentionally tiny —
//! the full mock plugin suite lands in #66. Behaviour is driven by the
//! `initialize` config so one binary can play every plugin kind:
//!
//! - `initialize` → stores the config; replies with a version and capabilities
//!   (`"no_state_stream": true` drops the `state_stream` capability). The full
//!   params are recorded to the config's `"init_log"` file, if set — separate
//!   from `notify_log`, which tests read as "observable side effects".
//! - `config/validate` → valid unless the config contains `"invalid": true`.
//! - `task/update_status` / `result/publish` → acknowledge (recorded to the
//!   config's `"notify_log"` file, if set, as `{"method": ..., "params": ...}`).
//! - `task/dispatch` → replies with the config's `"session_id"` (default
//!   `sess-mock`); `"commit_on_dispatch": true` makes the mock agent branch
//!   (`"branch_on_dispatch"`, default `feat/mock-agent-work`) and leave a real
//!   commit on it, so the pull_request output policy has something to push —
//!   the worktree arrives detached, so the branch has to come first;
//!   `"dirty_on_dispatch": true` leaves an uncommitted file so cleanup's
//!   data-loss guard (F-23 DirtySkipped) is exercisable;
//!   `"crash_on_dispatch": true` exits mid-dispatch (crash isolation, §5.3);
//!   `"dispatch_error": { code, message, only_when_resuming }` answers with an
//!   arbitrary JSON-RPC error instead (see `forces_dispatch_error`).
//! - `session/attach` → `attached: false` if the session id contains `gone`,
//!   otherwise `attached: true` with a state chosen from the id (`waiting`,
//!   `done`, `fail`, else `running`) so recovery paths are testable (#57).
//! - `state/subscribe` → emits one `state/notification` per entry of the
//!   config's `"stream_states"` array (default `["running"]`) for the
//!   subscribed session, then acknowledges.
//! - `session/release` → recorded to `"dispatch_log"`; `released: false` when
//!   the session id contains `gone` (pane already closed), else `true`.
//! - `session/list` → recorded to `"dispatch_log"`; returns the config's
//!   `"list_sessions"` array verbatim as `sessions` (default `[]`), so tests
//!   can stage orphan panes (#211).
//! - `notify` (notification) → appended to the `"notify_log"` file, if set.
//! - `task/cancel` → acknowledges.
//! - `crash` → exits immediately with code 1 (to test crash isolation).
//! - `shutdown` → replies, then exits 0.
//! - anything else → method-not-found error.
//!
//! Plugin-initiated requests (0.1.6): if the config has a `"request_on_init"`
//! object, it is emitted **verbatim** as one NDJSON line right after the
//! `initialize` reply (tests supply a full JSON-RPC request, e.g.
//! `task/submit`). Any incoming line with an `id` but no `method` — the
//! orchestrator's response to that request — is recorded to `notify_log` as
//! `{"method": "response", "params": <the response object>}`.

use std::io::{BufRead, Write};

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code};
use plugin_protocol::methods::{
    AgentState, ConfigValidateResult, DiagnosticsSnapshotResult, InitializeResult,
    SessionAttachResult, TaskDispatchResult,
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
        // A line with an `id` but no `method` is the orchestrator's response
        // to a request this mock initiated (0.1.6 `request_on_init`); record
        // it for tests instead of answering it.
        if request.get("method").is_none() {
            record(&config, "response", &request);
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
                // Hook-capability flags (0.1.3): `resume_session` /
                // `diagnostics_snapshot` make the orchestrator take the
                // hook-dispatch path (job_id + HookLaunchSpec). 0.1.4:
                // `pane_control` gates the `session/focus` control path (F-94).
                let flag = |k: &str| config.get(k).and_then(Value::as_bool).unwrap_or(false);
                Response::result(
                    request_id(&id),
                    serde_json::to_value(InitializeResult {
                        plugin_version: semver::Version::new(0, 1, 0),
                        capabilities: Capabilities {
                            plan_mode: true,
                            state_stream,
                            pane_control: flag("pane_control"),
                            resume_session: flag("resume_session"),
                            diagnostics_snapshot: flag("diagnostics_snapshot"),
                            // 0.1.6: a push task source (never polled).
                            task_submit: flag("task_submit"),
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
            "task/update_status" | "result/publish" => {
                record(&config, method, &params);
                Response::result(request_id(&id), Value::Null)
            }
            // `dispatch_error` (#261): answer with an arbitrary JSON-RPC error
            // instead of a session id — the only way to drive the
            // orchestrator's *code-specific* dispatch paths from a test. The
            // attempt is still recorded, because what those tests assert is the
            // sequence (a failed dispatch with `resume_session_id`, then the
            // retry without it).
            "task/dispatch" if forces_dispatch_error(&config, &params) => {
                record_to(config.get("dispatch_log"), "task/dispatch", &params);
                Response::error(request_id(&id), forced_dispatch_error(&config))
            }
            "task/dispatch" => {
                // Record the dispatch params (job_id / hook launch spec) so
                // integration tests can assert the hook-dispatch wiring.
                record_to(config.get("dispatch_log"), "task/dispatch", &params);
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
                // `commit_on_dispatch: true` makes the mock agent do what a
                // real one is asked to: name a branch, switch to it, and
                // commit. The worktree arrives detached, so the branch has to
                // come first — without it the commits land on a detached HEAD
                // and the orchestrator has nothing to push (the agent's work
                // ends at the commit, F-86).
                if config
                    .get("commit_on_dispatch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && let Some(worktree) = params.get("worktree_path").and_then(Value::as_str)
                {
                    let branch = config
                        .get("branch_on_dispatch")
                        .and_then(Value::as_str)
                        .unwrap_or(MOCK_BRANCH);
                    branch_in(worktree, branch);
                    commit_in(worktree);
                }
                // `dirty_on_dispatch: true` leaves an uncommitted file in the
                // worktree, so cleanup's data-loss guard (F-23 DirtySkipped)
                // is exercisable end to end.
                if config
                    .get("dirty_on_dispatch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && let Some(worktree) = params.get("worktree_path").and_then(Value::as_str)
                    && let Err(e) =
                        std::fs::write(std::path::Path::new(worktree).join("wip.txt"), b"wip")
                {
                    eprintln!("mock_plugin: dirty_on_dispatch failed in {worktree}: {e}");
                }
                // `hook_post_on_dispatch`: simulate a hook-capable Claude Code
                // agent self-reporting completion over the *real* UDS socket
                // (#141 E2E). Reads the launch-spec env the orchestrator injected
                // (TOTSUKA_HOOK_ENDPOINT / TOTSUKA_HOOK_TOKEN / TOTSUKA_JOB_ID)
                // and POSTs a synthetic Stop, exactly as `on-stop.sh` would.
                hook_post_on_dispatch(&config, &params, &session_id);
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
            "diagnostics/snapshot" => {
                // A pane screen capture for escalation diagnostics (R-10). The
                // text is config-driven so tests can assert it lands in the
                // audit detail; `null` simulates an unavailable pane.
                let text = config
                    .get("snapshot_text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(Some("╭─ mock pane ─╮".to_string()));
                Response::result(
                    request_id(&id),
                    serde_json::to_value(DiagnosticsSnapshotResult { text }).unwrap(),
                )
            }
            "session/focus" => {
                // The click-to-focus chain (F-94). Recorded so tests can assert
                // the orchestrator delegated with the opaque session id; a
                // session id containing `gone` simulates a vanished pane
                // (`focused: false`, not an error — same convention as
                // `session/attach`).
                record_to(config.get("dispatch_log"), "session/focus", &params);
                let focused = !params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .contains("gone");
                Response::result(request_id(&id), serde_json::json!({ "focused": focused }))
            }
            "session/release" => {
                // The cleanup pane-release chain (#210). Recorded so tests can
                // assert the orchestrator released the pane before removing the
                // worktree; a session id containing `gone` simulates a pane
                // that is already closed (`released: false`, not an error —
                // same convention as `session/focus`).
                record_to(config.get("dispatch_log"), "session/release", &params);
                let released = !params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .contains("gone");
                Response::result(request_id(&id), serde_json::json!({ "released": released }))
            }
            "session/list" => {
                // Orphan-pane detection (#211): the staged pane inventory
                // comes straight from config so tests control what doctor
                // sees.
                record_to(config.get("dispatch_log"), "session/list", &params);
                let sessions = config
                    .get("list_sessions")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                Response::result(request_id(&id), serde_json::json!({ "sessions": sessions }))
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

        // 0.1.6: after the initialize reply, emit the configured
        // plugin-initiated request (verbatim) so tests can drive the host's
        // incoming-request path.
        if method == "initialize" {
            if let Some(request) = config.get("request_on_init") {
                let _ = writeln!(stdout, "{}", serde_json::to_string(request).unwrap());
                let _ = stdout.flush();
            }
            // `submit_tasks`: one `task/submit` request per entry (0.1.6),
            // ids `submit-0`, `submit-1`, …. Repeating the same task tests
            // orchestrator-side idempotency (the second ack is `duplicate`).
            if let Some(tasks) = config.get("submit_tasks").and_then(Value::as_array) {
                for (i, task) in tasks.iter().enumerate() {
                    let request = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": format!("submit-{i}"),
                        "method": "task/submit",
                        "params": { "task": task },
                    });
                    let _ = writeln!(stdout, "{}", serde_json::to_string(&request).unwrap());
                    let _ = stdout.flush();
                }
            }
        }
    }
}

/// Simulate a hook-capable Claude Code agent self-reporting completion over the
/// real UDS hook socket (#141 E2E). No-op unless `hook_post_on_dispatch` is set
/// in the init config. The synthetic Stop is shaped exactly like the JSON
/// `on-stop.sh` emits, and is addressed with the `job_id` / endpoint / token the
/// orchestrator injected into `params.hook.env` (#132 `HookLaunchSpec`).
///
/// Config shape (all fields optional):
/// ```json
/// { "hook_post_on_dispatch": {
///     "status": "COMPLETED",              // COMPLETED | NEEDS_INPUT | FAILED | UNKNOWN
///     "message": "done <<STATUS:COMPLETED>>",
///     "prompt_id": "p-1",                 // vary to defeat idempotency dedup
///     "session_start": true               // fire SessionStart first (#242)
///   } }
/// ```
fn hook_post_on_dispatch(config: &Value, params: &Value, session_id: &str) {
    let Some(spec) = config.get("hook_post_on_dispatch") else {
        return;
    };
    let env = params.get("hook").and_then(|h| h.get("env"));
    let field = |key: &str| env.and_then(|e| e.get(key)).and_then(Value::as_str);
    let Some(endpoint) = field("TOTSUKA_HOOK_ENDPOINT") else {
        eprintln!(
            "mock_plugin: hook_post_on_dispatch set but no TOTSUKA_HOOK_ENDPOINT in hook env"
        );
        return;
    };
    let job_id = field("TOTSUKA_JOB_ID").unwrap_or("");
    let token = field("TOTSUKA_HOOK_TOKEN");

    let status = spec
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("COMPLETED");
    let message = spec
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("done <<STATUS:COMPLETED>>");
    let prompt_id = spec
        .get("prompt_id")
        .and_then(Value::as_str)
        .unwrap_or("p-mock");
    // `repeat` (default 1): POST the identical signal N times to exercise the
    // receiver + engine idempotency (D-05) through the real socket.
    let repeat = spec
        .get("repeat")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    // `session_start` (default false): fire the SessionStart hook first, as a
    // real agent CLI does. That is what establishes `sessions.tool_session_id`
    // — without it a task finishes with an unestablished session and nothing
    // downstream can resume it (#242).
    if spec
        .get("session_start")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let start = serde_json::json!({
            "job_id": job_id,
            "session_id": session_id,
            "hook_event_name": "SessionStart",
        })
        .to_string();
        if let Err(e) = post_uds(endpoint, token, &start) {
            eprintln!("mock_plugin: SessionStart POST to {endpoint} failed: {e}");
        }
    }
    let body = serde_json::json!({
        "job_id": job_id,
        "session_id": session_id,
        "prompt_id": prompt_id,
        "hook_event_name": "Stop",
        "status": status,
        "last_assistant_message": message,
        "background_tasks": [],
    })
    .to_string();
    for _ in 0..repeat {
        if let Err(e) = post_uds(endpoint, token, &body) {
            eprintln!("mock_plugin: hook POST to {endpoint} failed: {e}");
        }
    }
}

/// POST `body` to `POST /agent-events` on the UDS at `endpoint` (minimal
/// HTTP/1.1, `Connection: close`), mirroring `on-stop.sh`'s `curl --unix-socket`.
#[cfg(unix)]
fn post_uds(endpoint: &str, token: Option<&str>, body: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /agent-events HTTP/1.1\r\n\
         Host: localhost\r\n\
         {auth}\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    // Drain the reply so the server sees a clean close.
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
    Ok(())
}

#[cfg(not(unix))]
fn post_uds(_endpoint: &str, _token: Option<&str>, _body: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "UDS hook POST is only supported on Unix",
    ))
}

/// Whether this `task/dispatch` should be answered with a canned error
/// (`dispatch_error` in the init config).
///
/// Config shape (all fields optional):
/// ```json
/// { "dispatch_error": {
///     "code": -32006,                 // default -32603 (INTERNAL_ERROR)
///     "message": "session is gone",
///     "only_when_resuming": true      // default false = fail every dispatch
///   } }
/// ```
///
/// `only_when_resuming` is what makes the orchestrator's `SESSION_UNRESUMABLE`
/// retry testable (#242/#261): the attempt carrying `resume_session_id` fails,
/// and the retry — which names no session — falls through to the normal
/// dispatch and succeeds.
fn forces_dispatch_error(config: &Value, params: &Value) -> bool {
    let Some(spec) = config.get("dispatch_error") else {
        return false;
    };
    let only_when_resuming = spec
        .get("only_when_resuming")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    !only_when_resuming || params.get("resume_session_id").is_some()
}

/// The error [`forces_dispatch_error`] asked for.
fn forced_dispatch_error(config: &Value) -> Error {
    let field = |key: &str| config.get("dispatch_error").and_then(|s| s.get(key));
    Error::new(
        field("code")
            .and_then(Value::as_i64)
            .unwrap_or(error_code::INTERNAL_ERROR),
        field("message")
            .and_then(Value::as_str)
            .unwrap_or("mock plugin was configured to fail this dispatch"),
    )
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
/// The branch the mock agent picks when the config does not name one. Chosen
/// to look like something a repository convention would produce, because that
/// is what the real instruction asks for.
const MOCK_BRANCH: &str = "feat/mock-agent-work";

/// Stand in for the agent's own `git switch -c <name>`: the worktree is handed
/// over detached and naming the branch is the agent's job (it is the only
/// party that can read the repository's convention). Idempotent across a
/// re-dispatch into the same worktree — a second `-c` fails, and switching to
/// the branch that is already checked out succeeds trivially.
fn branch_in(worktree: &str, branch: &str) {
    let switched = std::process::Command::new("git")
        .current_dir(worktree)
        .args(["switch", "-c", branch])
        .output();
    match switched {
        Ok(out) if out.status.success() => {}
        _ => {
            let _ = std::process::Command::new("git")
                .current_dir(worktree)
                .args(["switch", branch])
                .output();
        }
    }
}

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
