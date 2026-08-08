//! JSON-RPC dispatch for the stdio server (F-51). Generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize` and `config/validate` — is driven in tests with a recorded
//! transport, no network involved.
//!
//! This is a **push source** (`task_submit`, since protocol 0.1.6): the SDK
//! [`poll_loop`] fetches every `initialize`-supplied trigger on an internal
//! cadence (`poll_interval_secs`, default 60s) and pushes each task via
//! `task/submit` (ADR-0008). `tasks/fetch` no longer exists as of protocol
//! 0.2.0.

use std::sync::Arc;
use std::time::Duration;

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, TriggerInfo,
};
use plugin_protocol::{Capabilities, OutputCapability, RequestId, method};
use plugin_sdk::{LineHandler, Reply, SubmitClient, poll_loop};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::{NotionClient, static_config_errors};
use crate::config::NotionConfig;
use crate::transport::{NotionTransport, TransportSettings};

/// The internal fetch cadence when the orchestrator supplies no
/// `poll_interval_secs` (F-06's default, now applied plugin-side).
const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Builds a transport from resolved connection settings. Abstracted so the
/// server can be tested with a recorded transport.
pub trait TransportFactory {
    /// The transport this factory produces.
    type Transport: NotionTransport;
    /// Build a transport from connection `settings`.
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport;
}

/// Connection settings derived from a [`NotionConfig`].
fn settings(config: &NotionConfig) -> TransportSettings<'_> {
    TransportSettings {
        api_url: &config.api_url,
        token: &config.token,
        api_version: &config.api_version,
        max_retries: config.max_retries,
        rate_limit_rps: config.rate_limit_rps,
    }
}

/// The Notion task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    /// The `task/submit` client the poll loop pushes through (0.1.6).
    submit: SubmitClient,
    /// Set by a successful `initialize`.
    session: Option<Session<F::Transport>>,
}

/// An initialized plugin session: the client plus the resident poll loop.
struct Session<T> {
    /// The REST client host-driven methods delegate to (the poll loop holds
    /// its own Arc clone).
    client: Arc<NotionClient<T>>,
    /// The `poll_loop` task (absent when `initialize` supplied no triggers —
    /// nothing to watch, nothing to poll).
    poll: Option<tokio::task::AbortHandle>,
}

impl<T> Drop for Session<T> {
    fn drop(&mut self) {
        // A replaced (re-initialize) or ended session must not leak a
        // resident task that keeps polling the API.
        if let Some(poll) = &self.poll {
            poll.abort();
        }
    }
}

impl<F: TransportFactory> Server<F>
where
    F::Transport: Send + Sync + 'static,
{
    /// A fresh, uninitialized server using `factory` to build transports and
    /// `submit` to push tasks (0.1.6).
    pub fn new(factory: F, submit: SubmitClient) -> Self {
        Self {
            factory,
            submit,
            session: None,
        }
    }

    /// Parse one NDJSON line, dispatch it, and produce a reply. A non-JSON line
    /// yields a `PARSE_ERROR` response with a null id; blank lines and
    /// notifications (no `id`) produce no response.
    pub async fn handle_line(&mut self, line: &str) -> Reply {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Reply::none();
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Reply::respond(Response::error_without_id(Error::new(
                error_code::PARSE_ERROR,
                "request was not valid JSON",
            )));
        };
        // A message without an `id` is a notification: never answered.
        let Some(id) = value.get("id").map(request_id) else {
            return Reply::none();
        };
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        self.dispatch(id, method, params).await
    }

    async fn dispatch(&mut self, id: RequestId, method: &str, params: Value) -> Reply {
        match method {
            method::INITIALIZE => self.initialize(id, params),
            method::CONFIG_VALIDATE => self.config_validate(id, params).await,
            method::SHUTDOWN => Reply::shutdown_ack(id),
            method::TASK_UPDATE_STATUS => self.update_status(id, params).await,
            method::RESULT_PUBLISH => self.result_publish(id, params).await,
            other => Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method: {other}"),
                ),
            )),
        }
    }

    /// `initialize`: deserialize the config, build the client, then start the
    /// resident [`poll_loop`] over the supplied triggers — each tick fetches
    /// every trigger and pushes the matching tasks via `task/submit` (0.1.6).
    fn initialize(&mut self, id: RequestId, params: Value) -> Reply {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let config: NotionConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid notion plugin config: {e}"),
                    ),
                ));
            }
        };
        let transport = self.factory.build(settings(&config));
        let client = Arc::new(NotionClient::new(config, transport));
        let poll = if init.triggers.is_empty() {
            None
        } else {
            // 0 would make the loop spin without sleeping (API hammering);
            // fall back to the default rather than honoring it.
            let secs = match init.poll_interval_secs {
                Some(0) => {
                    tracing::warn!(
                        "poll_interval_secs = 0 would busy-spin the poll loop → \
                         using the default ({DEFAULT_POLL_INTERVAL_SECS}s)"
                    );
                    DEFAULT_POLL_INTERVAL_SECS
                }
                Some(secs) => secs,
                None => DEFAULT_POLL_INTERVAL_SECS,
            };
            let interval = Duration::from_secs(secs);
            let fetch_client = Arc::clone(&client);
            let handle = tokio::spawn(poll_loop(
                init.triggers,
                interval,
                self.submit.clone(),
                move |trigger: &TriggerInfo| {
                    let client = Arc::clone(&fetch_client);
                    let condition = trigger.trigger.clone();
                    async move { client.fetch(&condition).await.map_err(|e| e.to_string()) }
                },
            ));
            Some(handle.abort_handle())
        };
        self.session = Some(Session { client, poll });
        Reply::respond(Response::result(id, capabilities_result()))
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let parsed: ConfigValidateParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let config: NotionConfig = match serde_json::from_value(parsed.config) {
            Ok(c) => c,
            Err(e) => return ok_validate(id, vec![format!("config does not parse: {e}")]),
        };
        let mut errors = static_config_errors(&config);
        // Only ping the API if the config is otherwise well-formed (F-63).
        if errors.is_empty() {
            let transport = self.factory.build(settings(&config));
            if let Err(e) = NotionClient::new(config, transport).validate().await {
                errors.push(e.to_string());
            }
        }
        ok_validate(id, errors)
    }

    async fn update_status(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return not_initialized(id);
        };
        let parsed: TaskUpdateStatusParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match session
            .client
            .update_status(&parsed.task_id, &parsed.status)
            .await
        {
            Ok(()) => Reply::respond(Response::result(id, Value::Null)),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
    }

    async fn result_publish(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return not_initialized(id);
        };
        let parsed: ResultPublishParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        // #398: the deliverable is the agent's to write now, through the Notion
        // MCP server. Warned **on use**, not at `initialize`: a config that
        // never reaches this path is not affected, and a startup warning it
        // cannot act on is noise.
        tracing::warn!(
            task_id = %parsed.task_id,
            "`result/publish` on this plugin is deprecated → the agent writes the \
             deliverable itself through the Notion MCP server (#398). Set the workflow's \
             `profile` to design/implement and drop `output = \"source\"`; this handler \
             and the Markdown→blocks conversion are removed in 0.3"
        );
        match session
            .client
            .publish(&parsed.task_id, &parsed.content, parsed.format.as_deref())
            .await
        {
            Ok(()) => Reply::respond(Response::result(id, Value::Null)),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
    }
}

/// Drive the server from the SDK stdio runtime (`plugin_sdk::serve`), which
/// also routes `task/submit` acks back to the shared [`SubmitClient`].
impl<F> LineHandler for Server<F>
where
    F: TransportFactory + Send,
    F::Transport: Send + Sync + 'static,
{
    async fn handle_line(&mut self, line: &str) -> Reply {
        Server::handle_line(self, line).await
    }
}

/// The capabilities this plugin declares (F-33/F-83): a **push** task source
/// (`task_submit`, 0.1.6 — never polled by the orchestrator) that can write
/// results back to the source (`result/publish`).
fn capabilities_result() -> Value {
    let result = InitializeResult {
        plugin_version: plugin_version(),
        capabilities: Capabilities {
            task_submit: true,
            outputs: vec![OutputCapability::Source],
            ..Capabilities::default()
        },
    };
    serde_json::to_value(result).unwrap_or(Value::Null)
}

/// This plugin's version, from Cargo. Falls back to `0.0.0` if unparseable.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(semver::Version::new(0, 0, 0))
}

/// A carrier used before an id is available (params-parse failures).
struct DeferredError {
    error: Error,
}

impl DeferredError {
    fn with_id(self, id: RequestId) -> Reply {
        Reply::respond(Response::error(id, self.error))
    }
}

/// Deserialize params, returning a deferred INVALID_PARAMS error on failure.
fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, DeferredError> {
    serde_json::from_value(params.clone()).map_err(|e| DeferredError {
        error: Error::new(error_code::INVALID_PARAMS, format!("invalid params: {e}")),
    })
}

/// A `config/validate` success reply (the RPC itself succeeds; validity is in
/// the payload).
fn ok_validate(id: RequestId, errors: Vec<String>) -> Reply {
    let result = ConfigValidateResult {
        valid: errors.is_empty(),
        errors,
    };
    Reply::respond(Response::result(
        id,
        serde_json::to_value(result).unwrap_or(Value::Null),
    ))
}

/// The error for a task_source method invoked before `initialize`.
fn not_initialized(id: RequestId) -> Reply {
    Reply::respond(Response::error(
        id,
        Error::new(
            error_code::INVALID_REQUEST,
            "plugin not initialized → send `initialize` first",
        ),
    ))
}

/// Map a [`crate::error::NotionError`] to a JSON-RPC error carrying its
/// actionable message.
fn rpc_error(id: RequestId, error: &crate::error::NotionError) -> Response {
    Response::error(
        id,
        Error::new(error_code::INTERNAL_ERROR, error.to_string()),
    )
}

/// Convert a JSON id value into a [`RequestId`]. The host uses numeric ids; a
/// string id round-trips as-is, and any other JSON scalar is preserved via its
/// textual form rather than collapsing to an empty string, so the caller can
/// still correlate the reply.
fn request_id(id: &Value) -> RequestId {
    if let Some(n) = id.as_i64() {
        RequestId::Number(n)
    } else if let Some(s) = id.as_str() {
        RequestId::Str(s.to_string())
    } else {
        RequestId::Str(id.to_string())
    }
}
