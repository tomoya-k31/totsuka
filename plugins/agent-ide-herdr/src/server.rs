//! JSON-RPC dispatch for the stdio server (F-51), with streamed
//! `state/notification` push (F-38).
//!
//! Responses and notifications are written as NDJSON lines to an
//! [`mpsc`](tokio::sync::mpsc) channel — [`main`](../main.rs) drains it to
//! stdout, tests drain it to a buffer. Generic over a [`TransportFactory`] so
//! the whole surface is driven against a fake herdr.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code, to_line};
use plugin_protocol::methods::{
    ConfigValidateResult, DiagnosticsSnapshotParams, InitializeParams, InitializeResult,
    SessionAttachParams, SessionFocusParams, StateSubscribeParams, TaskCancelParams,
    TaskDispatchParams,
};
use plugin_protocol::{Capabilities, RequestId, method};
use semver::Version;
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
        // Completion detection is now hook-based (0.1.3, #131): an orchestrator
        // older than 0.1.3 does not issue the hook launch spec, so tasks would
        // never be reported complete (the state stream is a deadman only). This
        // is compatible on the wire (`^0.1`) but operationally broken, so warn.
        if init.protocol_version < Version::new(0, 1, 3) {
            tracing::warn!(
                orchestrator = %init.protocol_version,
                "orchestrator protocol < 0.1.3: hook-based completion is unavailable, so this \
                 plugin will not report task completion (only pane.exited failures). Upgrade the \
                 orchestrator to 0.1.3+."
            );
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
        match self.connect(&config).await {
            Ok(transport) => {
                self.agent = Some(HerdrAgent::new(transport, config));
                self.send(Response::result(id, capabilities_result()));
            }
            Err(e) => self.send(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, e.to_string()),
            )),
        }
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) {
        let config: HerdrConfig = match params
            .get("config")
            .cloned()
            .ok_or(())
            .and_then(|c| serde_json::from_value(c).map_err(|_| ()))
        {
            Ok(c) => c,
            Err(()) => {
                return self.ok_validate(id, vec!["config does not parse".into()]);
            }
        };
        let mut errors = Vec::new();
        // Connectivity is the meaningful check (F-59): can we reach herdr and
        // does it answer `ping`?
        match self.connect(&config).await {
            Ok(transport) => {
                if let Err(e) = transport.call("ping", json!({})).await {
                    errors.push(format!("herdr did not answer ping → {e}"));
                }
            }
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

/// The capabilities this plugin declares (F-33): plan mode, design preview,
/// pane control, a state stream, plus session resume and pane diagnostics
/// snapshots (0.1.3, #131). Must mirror `plugin.toml`.
fn capabilities_result() -> Value {
    to_value(&InitializeResult {
        plugin_version: plugin_version(),
        capabilities: Capabilities {
            plan_mode: true,
            design_preview: true,
            pane_control: true,
            state_stream: true,
            // 0.1.3: `--resume` re-opens an agent session (Slack thread
            // continuation), and `diagnostics/snapshot` captures the pane
            // screen for escalation diagnostics (R-10).
            resume_session: true,
            diagnostics_snapshot: true,
            outputs: Vec::new(),
        },
    })
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
fn rpc_error(id: RequestId, error: &HerdrError) -> Response {
    Response::error(
        id,
        Error::new(error_code::INTERNAL_ERROR, error.to_string()),
    )
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
