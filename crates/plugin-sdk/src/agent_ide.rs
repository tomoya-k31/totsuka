//! JSON-RPC dispatch for agent_ide plugins: a typed [`AgentIdeHandler`] and
//! the [`AgentIdeServer`] wrapper that turns it into a [`LineHandler`]
//! covering the whole wire protocol — the agent_ide twin of
//! [`TaskSourceHandler`](crate::TaskSourceHandler).
//!
//! The one thing this adds over the task_source side is the state stream:
//! `state/subscribe` is answered with an ACK **and then** a stream of
//! `state/notification`s (F-38). The handler only returns the receiving end
//! of that stream; the server owns the ordering, because getting it wrong is
//! invisible in a handler and only shows up as a host reading a notification
//! for a subscription it has not been told succeeded.

use plugin_protocol::jsonrpc::{Error, Notification, Response, error_code, to_line};
use plugin_protocol::method;
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, DiagnosticsSnapshotParams,
    DiagnosticsSnapshotResult, InitializeParams, InitializeResult, SessionAttachParams,
    SessionAttachResult, SessionFocusParams, SessionFocusResult, SessionListResult,
    SessionReleaseParams, SessionReleaseResult, StateNotification, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams, TaskDispatchResult,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::dispatch::{Reply, Request, parse_params, parse_request, respond, unknown_method};
use crate::runtime::{LineHandler, Writer};

/// The typed surface an agent_ide plugin implements; [`AgentIdeServer`]
/// turns it into a [`LineHandler`].
///
/// Methods the host calls unconditionally are required. The three gated on a
/// capability — `session/focus` and `session/list` on `pane_control`,
/// `diagnostics/snapshot` on `diagnostics_snapshot` — default to a
/// `METHOD_NOT_FOUND` refusal, the same rule as
/// [`TaskSourceHandler::task_claim`](crate::TaskSourceHandler::task_claim): a
/// handler that overrides one must declare the flag, and one that declares
/// the flag must override it.
pub trait AgentIdeHandler: Send {
    /// `initialize`: store config, answer version + capabilities.
    fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> impl Future<Output = Result<InitializeResult, Error>> + Send;

    /// `config/validate` (F-59).
    fn config_validate(
        &mut self,
        params: ConfigValidateParams,
    ) -> impl Future<Output = Result<ConfigValidateResult, Error>> + Send;

    /// `task/dispatch`: start (or resume) the agent for a task.
    fn task_dispatch(
        &mut self,
        params: TaskDispatchParams,
    ) -> impl Future<Output = Result<TaskDispatchResult, Error>> + Send;

    /// `session/attach`: re-attach a session after an Orchestrator restart.
    fn session_attach(
        &mut self,
        params: SessionAttachParams,
    ) -> impl Future<Output = Result<SessionAttachResult, Error>> + Send;

    /// `task/cancel`.
    fn task_cancel(
        &mut self,
        params: TaskCancelParams,
    ) -> impl Future<Output = Result<(), Error>> + Send;

    /// `state/subscribe` (F-38): return the stream of state notifications.
    /// [`AgentIdeServer`] ACKs the request first and only then forwards what
    /// arrives on the receiver as `state/notification`s, until it closes.
    fn state_subscribe(
        &mut self,
        params: StateSubscribeParams,
    ) -> impl Future<Output = Result<mpsc::UnboundedReceiver<StateNotification>, Error>> + Send;

    /// `session/release` (0.2.2).
    fn session_release(
        &mut self,
        params: SessionReleaseParams,
    ) -> impl Future<Output = Result<SessionReleaseResult, Error>> + Send;

    /// `session/focus` — gated on the `pane_control` capability.
    fn session_focus(
        &mut self,
        params: SessionFocusParams,
    ) -> impl Future<Output = Result<SessionFocusResult, Error>> + Send {
        let _ = params;
        async { Err(unsupported("session/focus", "pane_control")) }
    }

    /// `session/list` — gated on the `pane_control` capability. Takes no
    /// params: the request's `params` are not read, so a host that omits them
    /// is answered the same as one that sends `{}`.
    fn session_list(&mut self) -> impl Future<Output = Result<SessionListResult, Error>> + Send {
        async { Err(unsupported("session/list", "pane_control")) }
    }

    /// `diagnostics/snapshot` (R-10) — gated on the `diagnostics_snapshot`
    /// capability.
    fn diagnostics_snapshot(
        &mut self,
        params: DiagnosticsSnapshotParams,
    ) -> impl Future<Output = Result<DiagnosticsSnapshotResult, Error>> + Send {
        let _ = params;
        async { Err(unsupported("diagnostics/snapshot", "diagnostics_snapshot")) }
    }
}

/// The refusal a defaulted method answers with.
fn unsupported(method: &str, capability: &str) -> Error {
    Error::new(
        error_code::METHOD_NOT_FOUND,
        format!(
            "{method} is not supported by this plugin → do not declare the `{capability}` capability"
        ),
    )
}

/// Adapter: drive an [`AgentIdeHandler`] as a [`LineHandler`], writing
/// `state/notification`s through `writer` — the same shared writer
/// [`serve`](crate::serve) writes replies through, so a reply and a
/// notification never interleave mid-line.
pub struct AgentIdeServer<H> {
    /// The plugin's handler.
    pub handler: H,
    writer: Writer,
}

impl<H: AgentIdeHandler> AgentIdeServer<H> {
    /// Serve `handler`, streaming notifications through `writer`
    /// (production: `stdio().writer`).
    pub fn new(handler: H, writer: Writer) -> Self {
        Self { handler, writer }
    }
}

impl<H: AgentIdeHandler> LineHandler for AgentIdeServer<H> {
    async fn handle_line(&mut self, line: &str) -> Reply {
        let Request { id, method, params } = match parse_request(line) {
            Ok(request) => request,
            Err(reply) => return reply,
        };
        let handler = &mut self.handler;
        macro_rules! call {
            ($parse:ty, $call:ident) => {
                match parse_params::<$parse>(&params) {
                    Ok(p) => respond(id, handler.$call(p).await),
                    Err(error) => Reply::respond(Response::error(id, error)),
                }
            };
        }
        match method.as_str() {
            method::INITIALIZE => call!(InitializeParams, initialize),
            method::CONFIG_VALIDATE => call!(ConfigValidateParams, config_validate),
            method::TASK_DISPATCH => call!(TaskDispatchParams, task_dispatch),
            method::SESSION_ATTACH => call!(SessionAttachParams, session_attach),
            method::TASK_CANCEL => call!(TaskCancelParams, task_cancel),
            method::SESSION_RELEASE => call!(SessionReleaseParams, session_release),
            method::SESSION_FOCUS => call!(SessionFocusParams, session_focus),
            method::DIAGNOSTICS_SNAPSHOT => call!(DiagnosticsSnapshotParams, diagnostics_snapshot),
            method::SESSION_LIST => respond(id, handler.session_list().await),
            method::STATE_SUBSCRIBE => {
                let parsed = match parse_params::<StateSubscribeParams>(&params) {
                    Ok(p) => p,
                    Err(error) => return Reply::respond(Response::error(id, error)),
                };
                match handler.state_subscribe(parsed).await {
                    Ok(rx) => {
                        // The ACK goes out through the writer *here*, not as
                        // this call's `Reply`: `serve` writes the reply only
                        // after we return, by which time the forwarder below
                        // could already have written a notification.
                        if let Ok(ack) = to_line(&Response::result(id, Value::Null)) {
                            self.writer.send_line(ack);
                        }
                        tokio::spawn(forward(rx, self.writer.clone()));
                        Reply::none()
                    }
                    Err(error) => Reply::respond(Response::error(id, error)),
                }
            }
            method::SHUTDOWN => Reply::shutdown_ack(id),
            other => unknown_method(id, other),
        }
    }
}

/// Forward state notifications until the stream or the writer closes.
async fn forward(mut rx: mpsc::UnboundedReceiver<StateNotification>, writer: Writer) {
    while let Some(note) = rx.recv().await {
        let notif = Notification::new(
            method::STATE_NOTIFICATION,
            Some(serde_json::to_value(&note).unwrap_or(Value::Null)),
        );
        if let Ok(line) = to_line(&notif)
            && !writer.send_line(line)
        {
            break;
        }
    }
}
