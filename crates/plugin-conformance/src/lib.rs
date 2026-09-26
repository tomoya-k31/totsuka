//! # Plugin conformance kit (#767)
//!
//! Starts a plugin **binary**, talks NDJSON to it over stdio, and reports every
//! protocol rule it breaks. Nothing here touches the plugin's own types, so it
//! checks what the host actually sees — the binary's `main`, its wiring into
//! the SDK runtime, and the handler — and it works the same for a plugin that
//! does not use `plugin-sdk` at all.
//!
//! ```ignore
//! let violations = plugin_conformance::check(
//!     env!("CARGO_BIN_EXE_mytool"),
//!     concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
//!     &init,
//! );
//! assert!(violations.is_empty(), "{}", violations.join("\n"));
//! ```
//!
//! `init` is the smallest `initialize` params your plugin would accept, plus —
//! for a `task_source` — one workflow with a trigger you accept. The kit never
//! sends it as-is: a successful `initialize` would reach real services, so
//! every check stays on a path that ends before that. It only sends broken
//! copies (checks 5, 6 and 9 below).
//!
//! ## The checks
//!
//! Every kind:
//!
//! 1. Before `initialize`, every kind-specific request the host would send
//!    this plugin ([`HOST_REQUESTS`], filtered by the manifest's
//!    capabilities) is refused with `INVALID_REQUEST` — with well-formed
//!    params and with `{}`. A notifier instead answers nothing to an early
//!    `notify`. A request before `initialize` must say so rather than fail on
//!    an absent session.
//! 2. A line that is not JSON gets `PARSE_ERROR` with a `null` id: there is
//!    no id to correlate against, and a made-up one (an empty string) would
//!    match nothing the host sent.
//! 3. An unknown method gets `METHOD_NOT_FOUND`.
//! 4. A blank line and a notification get no answer — answering a
//!    notification puts a line on the wire the host is not waiting for.
//! 5. `initialize` with params of the wrong shape gets `INVALID_PARAMS`.
//!    Malformed params are a protocol problem; `CONFIG_INVALID` would send the
//!    operator to edit a file that is not the cause.
//! 6. `config/validate` on the given config plus one unknown top-level key
//!    answers `valid: false`, with an error naming the key.
//! 7. `shutdown` is answered and the process exits with status 0.
//! 8. The process exits when stdin reaches EOF.
//!
//! `task_source` only:
//!
//! 9. `initialize` with an unknown key in the first workflow's trigger fails
//!    with `CONFIG_INVALID`, and the message names the key. An unknown key is
//!    a typo, and dropping it *widens* the trigger instead of narrowing it
//!    (#574).
//!
//! Error **messages** are never compared, only codes: the wording is not part
//! of the protocol. The unknown key in 6 and 9 is the exception because it is
//! the one thing the operator needs from the error to fix their config.
//!
//! [`HOST_REQUESTS`]: plugin_protocol::methods::HOST_REQUESTS

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use plugin_protocol::methods::{ConfigValidateParams, HOST_REQUESTS};
use plugin_protocol::{InitializeParams, Manifest, PluginKind, error_code, method};
use serde_json::{Value, json};

/// How long any single answer, or an exit, may take. Each is milliseconds
/// when healthy; the margin is for a loaded CI runner, not for the plugin.
const WAIT: Duration = Duration::from_secs(5);

/// The key the kit injects into config (check 6) and trigger (check 9).
const UNKNOWN_KEY: &str = "__totsuka_conformance_unknown_key";

/// Run every check against `binary` and return one line per violation —
/// empty when the plugin conforms. `manifest` is its `plugin.toml`, the source
/// of its kind and capabilities.
pub fn check(
    binary: impl AsRef<Path>,
    manifest: impl AsRef<Path>,
    init: &InitializeParams,
) -> Vec<String> {
    let manifest = match fs::read_to_string(manifest.as_ref())
        .map_err(|e| e.to_string())
        .and_then(|text| Manifest::from_toml_str(&text).map_err(|e| e.to_string()))
    {
        Ok(m) => m,
        Err(e) => return vec![format!("plugin.toml: {e}")],
    };
    let binary = binary.as_ref();
    let mut violations = Vec::new();

    match Session::spawn(binary) {
        Ok(mut session) => {
            if let Err(stop) = before_initialize(&mut session, &manifest, init, &mut violations) {
                violations.push(format!(
                    "{stop} — the remaining checks on this process were skipped"
                ));
            }
        }
        Err(e) => return vec![format!("cannot start {}: {e}", binary.display())],
    }

    // Check 8 needs a process of its own: closing stdin is final.
    match Session::spawn(binary) {
        Ok(mut session) => {
            drop(session.stdin.take());
            if session.exit_within(WAIT).is_none() {
                violations.push(format!(
                    "[8] still running {WAIT:?} after stdin reached EOF"
                ));
            }
        }
        Err(e) => violations.push(format!("cannot start {}: {e}", binary.display())),
    }
    violations
}

/// Checks 1–7 and 9, in one process that is never initialized. An `Err` is
/// a lost conversation (no answer, or an answer to the wrong request), after
/// which nothing further on this process can be read reliably.
fn before_initialize(
    session: &mut Session,
    manifest: &Manifest,
    init: &InitializeParams,
    violations: &mut Vec<String>,
) -> Result<(), String> {
    // 4 (and 1 for a notifier): nothing may answer these, so the first line
    // back must be the answer to the probe that follows them (3).
    session.send("")?;
    session.send(&json!({"jsonrpc": "2.0", "method": "conformance/notification"}).to_string())?;
    if manifest.kind == PluginKind::Notifier {
        session.send(
            &json!({"jsonrpc": "2.0", "method": method::NOTIFY, "params": sample_params(method::NOTIFY)})
                .to_string(),
        )?;
    }
    session.send(
        &json!({"jsonrpc": "2.0", "id": "unknown-method", "method": "conformance/no-such-method"})
            .to_string(),
    )?;
    let reply = session.next_line()?;
    if reply["id"] != "unknown-method" {
        return Err(format!(
            "[4] a blank line or a notification was answered: {reply}"
        ));
    }
    expect_error(
        violations,
        "[3] unknown method",
        &reply,
        error_code::METHOD_NOT_FOUND,
    );

    // 2
    session.send("{ this is not json")?;
    let reply = session.next_line()?;
    if !reply["id"].is_null() {
        violations.push(format!("[2] a parse error must carry a null id: {reply}"));
    }
    expect_error(
        violations,
        "[2] non-JSON line",
        &reply,
        error_code::PARSE_ERROR,
    );

    // 1
    let caps = &manifest.capabilities;
    for request in HOST_REQUESTS
        .iter()
        .filter(|r| r.kind == manifest.kind && (r.sent_to)(caps))
    {
        for params in [sample_params(request.method), json!({})] {
            let reply = session.request(request.method, params.clone())?;
            expect_error(
                violations,
                &format!("[1] `{}` {params} before initialize", request.method),
                &reply,
                error_code::INVALID_REQUEST,
            );
        }
    }

    // 5
    let reply = session.request(method::INITIALIZE, json!("not an object"))?;
    expect_error(
        violations,
        "[5] malformed initialize params",
        &reply,
        error_code::INVALID_PARAMS,
    );

    // 6
    match with_unknown_key(&init.config) {
        None => violations.push("[6] the given config is not a JSON object".into()),
        Some(config) => {
            let params = ConfigValidateParams {
                config,
                workflows: init.workflows.clone(),
                projects: init.projects.clone(),
                repositories: init.repositories.clone(),
            };
            let reply = session.request(
                method::CONFIG_VALIDATE,
                serde_json::to_value(&params).expect("protocol params serialize"),
            )?;
            let result = &reply["result"];
            if result["valid"] != false {
                violations.push(format!(
                    "[6] an unknown config key was not rejected: {reply}"
                ));
            } else if !result["errors"].to_string().contains(UNKNOWN_KEY) {
                violations.push(format!("[6] no error names the unknown key: {reply}"));
            }
        }
    }

    // 9
    if manifest.kind == PluginKind::TaskSource {
        let mut broken = init.clone();
        match broken
            .workflows
            .first_mut()
            .map(|w| with_unknown_key(&w.trigger))
        {
            None => violations.push("[9] the given params carry no workflow to break".into()),
            Some(None) => {
                violations.push("[9] the first workflow's trigger is not a JSON object".into())
            }
            Some(Some(trigger)) => {
                broken.workflows[0].trigger = trigger;
                let reply = session.request(
                    method::INITIALIZE,
                    serde_json::to_value(&broken).expect("protocol params serialize"),
                )?;
                if expect_error(
                    violations,
                    "[9] unknown trigger key",
                    &reply,
                    error_code::CONFIG_INVALID,
                ) && !reply["error"]["message"].to_string().contains(UNKNOWN_KEY)
                {
                    violations.push(format!(
                        "[9] the error does not name the unknown key: {reply}"
                    ));
                }
            }
        }
    }

    // 7
    let reply = session.request(method::SHUTDOWN, Value::Null)?;
    if reply.get("result").is_none() {
        violations.push(format!(
            "[7] shutdown was not answered with a result: {reply}"
        ));
    }
    match session.exit_within(WAIT) {
        None => violations.push(format!("[7] still running {WAIT:?} after shutdown")),
        Some(status) if !status.success() => {
            violations.push(format!("[7] exited with {status} after shutdown"));
        }
        Some(_) => {}
    }
    Ok(())
}

/// Record a violation unless `reply` is an error with `code`; true when it is.
fn expect_error(violations: &mut Vec<String>, what: &str, reply: &Value, code: i64) -> bool {
    let ok = reply["error"]["code"] == code;
    if !ok {
        violations.push(format!("{what}: expected error {code}, got {reply}"));
    }
    ok
}

/// `object` with [`UNKNOWN_KEY`] added, or `None` when it is not an object.
fn with_unknown_key(object: &Value) -> Option<Value> {
    let mut map = object.as_object()?.clone();
    map.insert(UNKNOWN_KEY.into(), Value::Bool(true));
    Some(Value::Object(map))
}

/// Well-formed params for each kind-specific request (and `notify`), so that
/// check 1 reaches the plugin's own "initialized?" gate instead of stopping at
/// a params error. The unit tests pin each to its typed params.
fn sample_params(method_name: &str) -> Value {
    let session = json!({"session_id": "w1:p1|conformance"});
    match method_name {
        method::TASK_UPDATE_STATUS => json!({"task_id": "42", "status": "done"}),
        method::TASK_CLAIM => json!({"task_id": "42"}),
        method::RESULT_PUBLISH => json!({"task_id": "42", "content": "x", "format": "markdown"}),
        method::TASK_DISPATCH => json!({
            "task": {"id": "42", "source": "conformance", "title": "x"},
            "worktree_path": "/nonexistent/conformance",
            "mode": "implement",
            "job_id": "job-1",
            "tool_launch": {"program": "true", "args": [], "env": {}}
        }),
        method::SESSION_RELEASE => json!({
            "session_id": "w1:p1|conformance",
            "expect_cwd": "/nonexistent/conformance",
            "expect_label": "conformance"
        }),
        method::TASK_CANCEL
        | method::SESSION_ATTACH
        | method::STATE_SUBSCRIBE
        | method::DIAGNOSTICS_SNAPSHOT
        | method::SESSION_FOCUS => session,
        method::SESSION_LIST => json!({}),
        method::NOTIFY => {
            json!({"event": "waiting_input", "task_id": "42", "title": "x", "body": "x"})
        }
        other => panic!("no sample params for `{other}` — add one next to its HOST_REQUESTS entry"),
    }
}

/// One running plugin process.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl Session {
    fn spawn(binary: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            lines,
            next_id: 0,
        })
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("stdin already closed")?;
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("the plugin stopped reading stdin: {e}"))
    }

    /// The next line the plugin writes, as JSON.
    fn next_line(&mut self) -> Result<Value, String> {
        let line = self.lines.recv_timeout(WAIT).map_err(|e| match e {
            RecvTimeoutError::Timeout => format!("no answer within {WAIT:?}"),
            RecvTimeoutError::Disconnected => match self.child.try_wait() {
                Ok(Some(status)) => format!("the plugin exited ({status}) instead of answering"),
                _ => "the plugin closed stdout instead of answering".to_string(),
            },
        })?;
        serde_json::from_str(&line)
            .map_err(|e| format!("the plugin wrote a non-JSON line ({e}): {line}"))
    }

    /// Send a request and read its answer, which must carry the same id.
    fn request(&mut self, method_name: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method_name});
        if !params.is_null() {
            request["params"] = params;
        }
        self.send(&request.to_string())?;
        let reply = self.next_line()?;
        if reply["id"] != id {
            return Err(format!(
                "`{method_name}` (id {id}) got an answer for another id: {reply}"
            ));
        }
        Ok(reply)
    }

    fn exit_within(&mut self, wait: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_protocol::rpc::{self, Method};
    use serde::de::DeserializeOwned;

    fn parses<M: Method>()
    where
        M::Params: DeserializeOwned,
    {
        serde_json::from_value::<M::Params>(sample_params(M::NAME))
            .unwrap_or_else(|e| panic!("sample params for `{}` do not parse: {e}", M::NAME));
    }

    /// Every table entry has a sample, and each sample is well-formed — or
    /// check 1 would test the params parser instead of the "initialized?"
    /// gate it is meant to reach.
    #[test]
    fn sample_params_are_well_formed() {
        parses::<rpc::TaskUpdateStatus>();
        parses::<rpc::TaskClaim>();
        parses::<rpc::ResultPublish>();
        parses::<rpc::TaskDispatch>();
        parses::<rpc::TaskCancel>();
        parses::<rpc::SessionAttach>();
        parses::<rpc::StateSubscribe>();
        parses::<rpc::DiagnosticsSnapshot>();
        parses::<rpc::SessionFocus>();
        parses::<rpc::SessionRelease>();
        parses::<rpc::SessionList>();
        serde_json::from_value::<plugin_protocol::methods::NotifyParams>(sample_params(
            method::NOTIFY,
        ))
        .expect("notify sample parses");
        for request in HOST_REQUESTS {
            sample_params(request.method); // panics when one is missing
        }
    }

    /// A kind with nothing to probe would pass check 1 vacuously.
    #[test]
    fn every_host_driven_kind_has_requests_to_probe() {
        for kind in [PluginKind::TaskSource, PluginKind::AgentIde] {
            assert!(HOST_REQUESTS.iter().any(|r| r.kind == kind), "{kind:?}");
        }
    }
}
