//! JSON-RPC dispatch for the stdio server (F-51), with streamed
//! `state/notification` push (F-38). Mirrors the herdr plugin's server — the
//! same methods and capabilities — but builds an orca CLI adapter instead of a
//! socket transport.
//!
//! Responses and notifications are written as NDJSON lines to an
//! [`mpsc`] channel — `main` drains it to
//! stdout, tests drain it to a buffer. Generic over a [`CliFactory`] so the
//! whole surface is driven against a fake orca CLI.

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code, to_line};
use plugin_protocol::methods::{
    ConfigValidateResult, DiagnosticsSnapshotParams, InitializeParams, InitializeResult,
    SessionAttachParams, SessionFocusParams, SessionReleaseParams, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams,
};
use plugin_protocol::{Capabilities, RequestId, method};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::agent::OrcaAgent;
use crate::cli::OrcaCli;
use crate::config::OrcaConfig;
use crate::error::OrcaError;

/// Builds an orca CLI adapter from config. Abstracted so the server is tested
/// against a fake orca.
pub trait CliFactory {
    /// The CLI adapter this factory produces.
    type Cli: OrcaCli;
    /// Build a CLI adapter for `config`.
    fn build(&self, config: &OrcaConfig) -> Self::Cli;
}

/// The orca agent_ide stdio server.
pub struct Server<F: CliFactory> {
    factory: F,
    agent: Option<OrcaAgent<F::Cli>>,
    out: mpsc::UnboundedSender<String>,
}

impl<F: CliFactory> Server<F> {
    /// A fresh, uninitialized server writing NDJSON lines to `out`.
    pub fn new(factory: F, out: mpsc::UnboundedSender<String>) -> Self {
        Self {
            factory,
            agent: None,
            out,
        }
    }

    /// Parse and dispatch one NDJSON line. Returns `false` when the server
    /// should exit (after `shutdown`).
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
            method::INITIALIZE => self.initialize(id, params),
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

    fn initialize(&mut self, id: RequestId, params: Value) {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(e) => return self.send(Response::error(id, e)),
        };
        let removed = crate::config::removed_keys_in(&init.config);
        if !removed.is_empty() {
            return self.send(Response::error(
                id,
                Error::new(
                    error_code::CONFIG_INVALID,
                    format!("invalid orca plugin config: {}", removed.join(" ")),
                ),
            ));
        }
        let config: OrcaConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return self.send(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid orca plugin config: {e}"),
                    ),
                ));
            }
        };
        let cli = self.factory.build(&config);
        self.agent = Some(OrcaAgent::new(cli, config));
        self.send(Response::result(id, capabilities_result()));
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) {
        let raw = params.get("config").cloned().unwrap_or(Value::Null);
        // Name the removed keys — `config does not parse` is true but useless
        // for the one change the tool_launch rewrite forces.
        let removed = crate::config::removed_keys_in(&raw);
        if !removed.is_empty() {
            return self.ok_validate(id, removed);
        }
        let config: OrcaConfig = match serde_json::from_value(raw) {
            Ok(c) => c,
            Err(_) => return self.ok_validate(id, vec!["config does not parse".into()]),
        };
        // Connectivity check (F-59): does `orca status` run, and is the
        // runtime behind it up? The CLI answers `status` even with the app
        // closed, so a successful call alone proves only that orca is
        // installed.
        let mut errors = Vec::new();
        let cli = self.factory.build(&config);
        match cli.run(vec!["status".into(), "--json".into()]).await {
            Ok(status) => {
                let reachable = status
                    .get("runtime")
                    .and_then(|r| r.get("reachable"))
                    .and_then(Value::as_bool);
                if reachable == Some(false) {
                    errors.push(
                        "the orca runtime is not reachable → start the Orca app (`orca open`)"
                            .into(),
                    );
                }
            }
            Err(e) => errors.push(format!("orca is not reachable → {e}")),
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

    fn ok_validate(&self, id: RequestId, errors: Vec<String>) {
        let result = ConfigValidateResult {
            valid: errors.is_empty(),
            errors,
            warnings: Vec::new(),
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

/// The capabilities this plugin declares (F-33) — the herdr plugin's set, now
/// that orca is driven through the same contract. Must mirror `plugin.toml`.
///
/// - `pane_control`: an orca terminal is the pane — `terminal switch` /
///   `close` / `list` answer focus, release and list.
/// - `hook_completion`: the agent is launched from the Orchestrator's
///   `tool_launch`, hook settings and env included, so it reports completion
///   through its hooks; the state stream is only an exit deadman.
/// - `diagnostics_snapshot`: `terminal read --screen`.
fn capabilities_result() -> Value {
    to_value(&InitializeResult {
        // No workflow options of its own (#554).
        claimed_options: Vec::new(),
        plugin_version: plugin_version(),
        claimed_repos: Vec::new(),
        capabilities: Capabilities {
            pane_control: true,
            state_stream: true,
            hook_completion: true,
            diagnostics_snapshot: true,
            ..Capabilities::default()
        },
    })
}

/// This plugin's version, from Cargo. Falls back to `0.0.0` if unparseable.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(semver::Version::new(0, 0, 0))
}

/// Serialize a value to JSON, falling back to `null` on the (unreachable) error.
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

/// Map an [`OrcaError`] to a JSON-RPC error carrying its actionable message:
/// an internal error, except a session that could not be resumed
/// (`SESSION_UNRESUMABLE`, #242 — the Orchestrator retries without it) and a
/// dispatch without a `tool_launch` (the caller's own malformed request).
fn rpc_error(id: RequestId, error: &OrcaError) -> Response {
    let code = match error {
        OrcaError::SessionUnresumable(_) => error_code::SESSION_UNRESUMABLE,
        OrcaError::MissingToolLaunch => error_code::INVALID_PARAMS,
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
