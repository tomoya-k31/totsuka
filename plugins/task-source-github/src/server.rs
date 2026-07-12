//! JSON-RPC dispatch for the stdio server (F-51). Generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize` and `config/validate` — is driven in tests with a recorded
//! transport, no network involved.

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, TasksFetchParams, TasksFetchResult,
};
use plugin_protocol::{Capabilities, OutputCapability, RequestId, method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::{GithubClient, static_config_errors};
use crate::config::GithubConfig;
use crate::transport::GithubTransport;

/// Builds a transport from resolved connection settings. Abstracted so the
/// server can be tested with a recorded transport.
pub trait TransportFactory {
    /// The transport this factory produces.
    type Transport: GithubTransport;
    /// Build a transport for `endpoint`, authenticating with `token`.
    fn build(&self, endpoint: &str, token: &str, max_retries: u32) -> Self::Transport;
}

/// The result of handling one input line.
pub struct Reply {
    /// The response line to write (absent for notifications, which get no reply).
    pub line: Option<String>,
    /// Whether the server should exit after this line (`shutdown`).
    pub shutdown: bool,
}

impl Reply {
    fn none() -> Self {
        Self {
            line: None,
            shutdown: false,
        }
    }
    fn respond(response: Response) -> Self {
        Self {
            line: plugin_protocol::jsonrpc::to_line(&response).ok(),
            shutdown: false,
        }
    }
}

/// The GitHub task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    client: Option<GithubClient<F::Transport>>,
}

impl<F: TransportFactory> Server<F> {
    /// A fresh, uninitialized server using `factory` to build transports.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            client: None,
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
            method::SHUTDOWN => Reply {
                line: plugin_protocol::jsonrpc::to_line(&Response::result(id, Value::Null)).ok(),
                shutdown: true,
            },
            method::TASKS_FETCH => self.tasks_fetch(id, params).await,
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

    fn initialize(&mut self, id: RequestId, params: Value) -> Reply {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let config: GithubConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid github plugin config: {e}"),
                    ),
                ));
            }
        };
        let transport = self
            .factory
            .build(&config.api_url, &config.token, config.max_retries);
        self.client = Some(GithubClient::new(config, transport));
        Reply::respond(Response::result(id, capabilities_result()))
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let parsed: ConfigValidateParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let config: GithubConfig = match serde_json::from_value(parsed.config) {
            Ok(c) => c,
            Err(e) => return ok_validate(id, vec![format!("config does not parse: {e}")]),
        };
        let mut errors = static_config_errors(&config);
        // Only ping the API if the config is otherwise well-formed (F-63).
        if errors.is_empty() {
            let transport = self
                .factory
                .build(&config.api_url, &config.token, config.max_retries);
            if let Err(e) = GithubClient::new(config, transport).validate().await {
                errors.push(e.to_string());
            }
        }
        ok_validate(id, errors)
    }

    async fn tasks_fetch(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(client) = self.client.as_ref() else {
            return not_initialized(id);
        };
        let parsed: TasksFetchParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match client.fetch(&parsed.trigger).await {
            Ok(tasks) => Reply::respond(Response::result(
                id,
                serde_json::to_value(TasksFetchResult { tasks }).unwrap_or(Value::Null),
            )),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
    }

    async fn update_status(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(client) = self.client.as_ref() else {
            return not_initialized(id);
        };
        let parsed: TaskUpdateStatusParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match client.update_status(&parsed.task_id, &parsed.status).await {
            Ok(()) => Reply::respond(Response::result(id, Value::Null)),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
    }

    async fn result_publish(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(client) = self.client.as_ref() else {
            return not_initialized(id);
        };
        let parsed: ResultPublishParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match client
            .publish(&parsed.task_id, &parsed.content, parsed.format.as_deref())
            .await
        {
            Ok(()) => Reply::respond(Response::result(id, Value::Null)),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
    }
}

/// The capabilities this plugin declares (F-33/F-83): a task source that can
/// write results back to the source (`result/publish`).
fn capabilities_result() -> Value {
    let result = InitializeResult {
        plugin_version: plugin_version(),
        capabilities: Capabilities {
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

/// Map a [`crate::error::GithubError`] to a JSON-RPC error carrying its
/// actionable message.
fn rpc_error(id: RequestId, error: &crate::error::GithubError) -> Response {
    Response::error(
        id,
        Error::new(error_code::INTERNAL_ERROR, error.to_string()),
    )
}

/// Convert a JSON id value into a [`RequestId`]. The host uses numeric ids; a
/// string id round-trips as-is, and any other JSON scalar (e.g. a float or an
/// out-of-`i64`-range number) is preserved via its textual form rather than
/// collapsing to an empty string, so the caller can still correlate the reply.
fn request_id(id: &Value) -> RequestId {
    if let Some(n) = id.as_i64() {
        RequestId::Number(n)
    } else if let Some(s) = id.as_str() {
        RequestId::Str(s.to_string())
    } else {
        RequestId::Str(id.to_string())
    }
}
