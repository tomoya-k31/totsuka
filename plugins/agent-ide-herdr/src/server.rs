//! JSON-RPC dispatch for the stdio server (F-51), with streamed
//! `state/notification` push (F-38).
//!
//! Responses and notifications are written as NDJSON lines to an
//! [`mpsc`] channel — `main` drains it to
//! stdout, tests drain it to a buffer. Generic over a [`TransportFactory`] so
//! the whole surface is driven against a fake herdr.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code, to_line};
use plugin_protocol::methods::{
    ConfigValidateResult, DiagnosticsSnapshotParams, InitializeParams, InitializeResult,
    SessionAttachParams, SessionFocusParams, SessionReleaseParams, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams,
};
use plugin_protocol::{Capabilities, RequestId, method};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agent::HerdrAgent;
use crate::config::HerdrConfig;
use crate::error::HerdrError;
use crate::transport::HerdrTransport;

/// Builds (connects) a herdr transport. Abstracted so the server is tested
/// against a fake herdr socket.
pub trait TransportFactory {
    /// The transport this factory produces.
    type Transport: HerdrTransport;
    /// Connect to the herdr socket at `path` with a per-request `timeout`.
    fn build(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> impl Future<Output = Result<Self::Transport, HerdrError>> + Send;
}

/// The herdr agent_ide stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    agent: Option<HerdrAgent<F::Transport>>,
    out: mpsc::UnboundedSender<String>,
}

impl<F: TransportFactory> Server<F> {
    /// A fresh, uninitialized server writing NDJSON lines to `out`.
    pub fn new(factory: F, out: mpsc::UnboundedSender<String>) -> Self {
        Self {
            factory,
            agent: None,
            out,
        }
    }

    /// Parse and dispatch one NDJSON line. Returns `false` when the server
    /// should exit (after `shutdown`). Responses/notifications are sent via the
    /// output channel; blank lines and notifications (no `id`) get no response.
    pub async fn handle_line(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return true;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            self.send(Response::error_without_id(Error::new(
                error_code::PARSE_ERROR,
                "request was not valid JSON",
            )));
            return true;
        };
        let Some(id) = value.get("id").map(request_id) else {
            return true; // a notification: never answered
        };
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        self.dispatch(id, method, params).await
    }

    async fn dispatch(&mut self, id: RequestId, method: &str, params: Value) -> bool {
        match method {
            method::INITIALIZE => self.initialize(id, params).await,
            method::CONFIG_VALIDATE => self.config_validate(id, params).await,
            method::TASK_DISPATCH => self.task_dispatch(id, params).await,
            method::SESSION_ATTACH => self.session_attach(id, params).await,
            method::TASK_CANCEL => self.task_cancel(id, params).await,
            method::STATE_SUBSCRIBE => self.state_subscribe(id, params).await,
            method::DIAGNOSTICS_SNAPSHOT => self.diagnostics_snapshot(id, params).await,
            method::SESSION_FOCUS => self.session_focus(id, params).await,
            method::SESSION_RELEASE => self.session_release(id, params).await,
            method::SESSION_LIST => self.session_list(id).await,
            method::SHUTDOWN => {
                self.send(Response::result(id, Value::Null));
                return false;
            }
            other => self.send(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method: {other}"),
                ),
            )),
        }
        true
    }

    async fn initialize(&mut self, id: RequestId, params: Value) {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        // No runtime version floor is checked here any more (#411). The
        // manifest declares `>=0.2.3`, so an orchestrator too old to send
        // `tool_launch` — or too old for hook-based completion (0.1.3, #131) —
        // is refused before `initialize` is even reached (F-54). A warning
        // here could only fire in a state the launcher already made
        // unreachable.
        let removed = crate::config::removed_keys_in(&init.config);
        if !removed.is_empty() {
            return self.send(Response::error(
                id,
                Error::new(
                    error_code::CONFIG_INVALID,
                    format!("invalid herdr plugin config: {}", removed.join(" ")),
                ),
            ));
        }
        let config: HerdrConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return self.send(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid herdr plugin config: {e}"),
                    ),
                ));
            }
        };
        let transport = match self.connect(&config).await {
            Ok(t) => t,
            Err(e) => {
                return self.send(Response::error(
                    id,
                    Error::new(error_code::CONFIG_INVALID, e.to_string()),
                ));
            }
        };
        // Refuse a herdr this plugin cannot drive, here rather than at the
        // first dispatch (ADR-0032 D-6). Before this check the symptom was
        // `invalid_request: missing field 'kind'` on a task that had already
        // been ingested, had a worktree cut for it, and failed — an error at
        // `initialize` is one `totsuka doctor` away instead.
        if let Err(e) = check_herdr_protocol(&transport).await {
            return self.send(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, e.to_string()),
            ));
        }
        self.agent = Some(HerdrAgent::new(transport, config));
        self.send(Response::result(id, capabilities_result()));
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) {
        let raw = params.get("config").cloned().unwrap_or(Value::Null);
        // Report the removed keys (#411) by name — `config does not parse`
        // below is true but useless for the one config change 0.4.0 forces.
        let removed = crate::config::removed_keys_in(&raw);
        if !removed.is_empty() {
            return self.ok_validate(id, removed);
        }
        let config: HerdrConfig = match serde_json::from_value(raw) {
            Ok(c) => c,
            Err(_) => {
                return self.ok_validate(id, vec!["config does not parse".into()]);
            }
        };
        let mut errors = Vec::new();
        // Connectivity is the meaningful check (F-59): can we reach herdr and
        // does it answer `ping`? Since ADR-0032 the same answer also carries
        // the protocol version, so the version check costs no extra round trip
        // and `totsuka config validate` reports a too-old herdr by name.
        match self.connect(&config).await {
            Ok(transport) => match transport.call("ping", json!({})).await {
                Ok(pong) => {
                    if let Err(e) = check_protocol_version(&pong) {
                        errors.push(e.to_string());
                    }
                }
                Err(e) => errors.push(format!("herdr did not answer ping → {e}")),
            },
            Err(e) => errors.push(e.to_string()),
        }
        self.ok_validate(id, errors);
    }

    async fn task_dispatch(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: TaskDispatchParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.dispatch(parsed).await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn session_attach(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: SessionAttachParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.attach(&parsed.session_id).await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn task_cancel(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: TaskCancelParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.cancel(&parsed.session_id).await {
            Ok(()) => self.send(Response::result(id, Value::Null)),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn state_subscribe(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: StateSubscribeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.start_state_stream(&parsed.session_id).await {
            Ok(mut rx) => {
                // ACK first, then forward mapped state notifications (F-38).
                self.send(Response::result(id, Value::Null));
                let out = self.out.clone();
                tokio::spawn(async move {
                    while let Some(note) = rx.recv().await {
                        let notif =
                            Notification::new(method::STATE_NOTIFICATION, Some(to_value(&note)));
                        if let Ok(line) = to_line(&notif)
                            && out.send(line).is_err()
                        {
                            break;
                        }
                    }
                });
            }
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn diagnostics_snapshot(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: DiagnosticsSnapshotParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.snapshot(&parsed.session_id).await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn session_focus(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: SessionFocusParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.focus(&parsed.session_id).await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn session_release(&mut self, id: RequestId, params: Value) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        let parsed: SessionReleaseParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        match agent.release(&parsed).await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    async fn session_list(&mut self, id: RequestId) {
        let Some(agent) = self.agent.as_ref() else {
            return self.send(not_initialized(id));
        };
        match agent.list_sessions().await {
            Ok(result) => self.send(Response::result(id, to_value(&result))),
            Err(e) => self.send(rpc_error(id, &e)),
        }
    }

    /// Connect a transport for `config` (resolving the socket path + timeout).
    async fn connect(&self, config: &HerdrConfig) -> Result<F::Transport, HerdrError> {
        let path = config.resolve_socket_path();
        let timeout = Duration::from_secs(config.request_timeout_secs);
        self.factory.build(&path, timeout).await
    }

    fn ok_validate(&self, id: RequestId, errors: Vec<String>) {
        let result = ConfigValidateResult {
            valid: errors.is_empty(),
            errors,
        };
        self.send(Response::result(id, to_value(&result)));
    }

    /// Serialize a response and enqueue it on the output channel.
    fn send(&self, response: Response) {
        if let Ok(line) = to_line(&response) {
            let _ = self.out.send(line);
        }
    }
}

/// The capabilities this plugin declares (F-33): plan mode, pane control, a
/// state stream, plus session resume and pane diagnostics snapshots (0.1.3,
/// #131). Must mirror `plugin.toml`.
///
/// `design_preview` used to be declared here and was removed with the field
/// itself in protocol 0.4.0 (#411/#356) — nothing ever read it, so declaring it
/// promised a feature that did not exist.
fn capabilities_result() -> Value {
    to_value(&InitializeResult {
        plugin_version: plugin_version(),
        capabilities: Capabilities {
            plan_mode: true,
            pane_control: true,
            state_stream: true,
            // 0.1.3: `--resume` re-opens an agent session (Slack thread
            // continuation), and `diagnostics/snapshot` captures the pane
            // screen for escalation diagnostics (R-10).
            resume_session: true,
            diagnostics_snapshot: true,
            ..Capabilities::default()
        },
    })
}

/// The oldest herdr Socket API protocol this plugin can drive
/// ([ADR-0032](../../../ai-docs/decisions/adr-0032-herdr-protocol-17.md) D-6).
///
/// 17 is where `agent.start` became manifest-driven and `agent.send` was
/// replaced by `agent.prompt`. Everything older needs the pre-ADR-0032 dispatch
/// path, which is not kept: a second path would be one that CI never runs
/// (herdr is not in CI, §9), and the two differ in pane ownership, env
/// injection and prompt submission at once — there is almost nothing to share.
const MIN_HERDR_PROTOCOL: u64 = 17;

/// Ask herdr its protocol version and refuse anything older than
/// [`MIN_HERDR_PROTOCOL`].
///
/// A `ping` that fails is **not** treated as a version problem: `connect`
/// already proved the socket, so a failure here is herdr misbehaving, and
/// reporting it as "upgrade herdr" would send the operator after the wrong
/// thing.
async fn check_herdr_protocol<T: HerdrTransport>(transport: &T) -> Result<(), HerdrError> {
    let pong = transport.call("ping", json!({})).await?;
    check_protocol_version(&pong)
}

/// The version half of [`check_herdr_protocol`], over a `ping` response.
///
/// **A `ping` with no `protocol` field passes.** The field has been there since
/// at least 0.7.1, so its absence means a herdr shaped differently from any this
/// plugin has seen — and refusing to start on an unknown shape would turn a
/// guess into an outage, while the dispatch path fails loudly on its own if the
/// guess was wrong.
fn check_protocol_version(pong: &Value) -> Result<(), HerdrError> {
    let Some(protocol) = pong.get("protocol").and_then(Value::as_u64) else {
        return Ok(());
    };
    if protocol < MIN_HERDR_PROTOCOL {
        return Err(HerdrError::InvalidResponse(format!(
            "herdr speaks protocol {protocol}, but this plugin needs {MIN_HERDR_PROTOCOL} or \
             newer (herdr 0.7.5+): protocol 17 changed `agent.start` and replaced `agent.send` \
             with `agent.prompt` → run `herdr update`, then `herdr status` to confirm"
        )));
    }
    Ok(())
}

/// This plugin's version, from Cargo. Falls back to `0.0.0` if unparseable.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(semver::Version::new(0, 0, 0))
}

/// Serialize a value to JSON, falling back to `null` on the (unreachable)
/// serialization error.
fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Deserialize params, returning an INVALID_PARAMS error on failure.
fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, Error> {
    serde_json::from_value(params.clone())
        .map_err(|e| Error::new(error_code::INVALID_PARAMS, format!("invalid params: {e}")))
}

/// The error for an agent_ide method invoked before `initialize`.
fn not_initialized(id: RequestId) -> Response {
    Response::error(
        id,
        Error::new(
            error_code::INVALID_REQUEST,
            "plugin not initialized → send `initialize` first",
        ),
    )
}

/// Map a [`HerdrError`] to a JSON-RPC error carrying its actionable message.
///
/// Everything herdr can go wrong with is an internal error to the caller —
/// except a session it could not resume, which the protocol gives its own code
/// so the Orchestrator can retry without it (0.2.4 `SESSION_UNRESUMABLE`,
/// #242), and a dispatch that arrived without a `tool_launch`, which is the
/// caller's own malformed request (#411).
fn rpc_error(id: RequestId, error: &HerdrError) -> Response {
    let code = match error {
        HerdrError::SessionUnresumable(_) => error_code::SESSION_UNRESUMABLE,
        HerdrError::MissingToolLaunch => error_code::INVALID_PARAMS,
        _ => error_code::INTERNAL_ERROR,
    };
    Response::error(id, Error::new(code, error.to_string()))
}

/// Convert a JSON id value into a [`RequestId`].
fn request_id(id: &Value) -> RequestId {
    if let Some(n) = id.as_i64() {
        RequestId::Number(n)
    } else if let Some(s) = id.as_str() {
        RequestId::Str(s.to_string())
    } else {
        RequestId::Str(id.to_string())
    }
}
