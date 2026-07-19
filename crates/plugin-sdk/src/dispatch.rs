//! JSON-RPC dispatch boilerplate shared by task_source plugins: the
//! [`Reply`] shape, id/params helpers, and a typed [`TaskSourceHandler`]
//! whose [`TaskSourceServer`] wrapper implements the full line protocol
//! (parse errors, notifications, `shutdown`, unknown methods).

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, TasksFetchParams, TasksFetchResult,
};
use plugin_protocol::{RequestId, method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::runtime::LineHandler;

/// The result of handling one input line.
pub struct Reply {
    /// The response line to write (absent for notifications, which get no
    /// reply).
    pub line: Option<String>,
    /// Whether the server should exit after this line (`shutdown`).
    pub shutdown: bool,
}

impl Reply {
    /// No output, keep serving (blank lines, notifications).
    pub fn none() -> Self {
        Self {
            line: None,
            shutdown: false,
        }
    }

    /// Encode `response` as the reply line.
    pub fn respond(response: Response) -> Self {
        Self {
            line: plugin_protocol::jsonrpc::to_line(&response).ok(),
            shutdown: false,
        }
    }

    /// Acknowledge `shutdown` and stop the serve loop.
    pub fn shutdown_ack(id: RequestId) -> Self {
        Self {
            line: plugin_protocol::jsonrpc::to_line(&Response::result(id, Value::Null)).ok(),
            shutdown: true,
        }
    }
}

/// Convert a JSON id value into a [`RequestId`] (number or string).
pub fn request_id(id: &Value) -> RequestId {
    match id.as_i64() {
        Some(n) => RequestId::Number(n),
        None => RequestId::Str(id.as_str().unwrap_or("").to_string()),
    }
}

/// Deserialize typed params, mapping failure to `INVALID_PARAMS`.
pub fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, Error> {
    serde_json::from_value(params.clone()).map_err(|e| {
        Error::new(
            error_code::INVALID_PARAMS,
            format!("invalid params: {e} → fix the request shape"),
        )
    })
}

/// The typed surface a task_source plugin implements; [`TaskSourceServer`]
/// turns it into a [`LineHandler`] covering the whole wire protocol.
///
/// `tasks_fetch` has a default empty implementation: push sources
/// (`task_submit` capability) are never polled, and the method is deprecated
/// since protocol 0.1.6 anyway.
pub trait TaskSourceHandler: Send {
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

    /// `task/update_status` (F-84). Return value is ignored by the host;
    /// `Value::Null` is conventional.
    fn update_status(
        &mut self,
        params: TaskUpdateStatusParams,
    ) -> impl Future<Output = Result<Value, Error>> + Send;

    /// `result/publish` (F-07).
    fn result_publish(
        &mut self,
        params: ResultPublishParams,
    ) -> impl Future<Output = Result<Value, Error>> + Send;

    /// `tasks/fetch` (deprecated since 0.1.6). Push sources keep the default
    /// empty answer; legacy sources may override during migration.
    fn tasks_fetch(
        &mut self,
        _params: TasksFetchParams,
    ) -> impl Future<Output = Result<TasksFetchResult, Error>> + Send {
        async { Ok(TasksFetchResult { tasks: vec![] }) }
    }
}

/// Adapter: drive a [`TaskSourceHandler`] as a [`LineHandler`].
pub struct TaskSourceServer<H>(pub H);

impl<H: TaskSourceHandler> LineHandler for TaskSourceServer<H> {
    async fn handle_line(&mut self, line: &str) -> Reply {
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
        macro_rules! call {
            ($parse:ty, $call:ident) => {
                match parse_params::<$parse>(&params) {
                    Ok(p) => match self.0.$call(p).await {
                        Ok(result) => match serde_json::to_value(result) {
                            Ok(v) => Reply::respond(Response::result(id, v)),
                            Err(e) => Reply::respond(Response::error(
                                id,
                                Error::new(
                                    error_code::INTERNAL_ERROR,
                                    format!("failed to encode result: {e}"),
                                ),
                            )),
                        },
                        Err(error) => Reply::respond(Response::error(id, error)),
                    },
                    Err(error) => Reply::respond(Response::error(id, error)),
                }
            };
        }
        match method {
            method::INITIALIZE => call!(InitializeParams, initialize),
            method::CONFIG_VALIDATE => call!(ConfigValidateParams, config_validate),
            method::TASK_UPDATE_STATUS => call!(TaskUpdateStatusParams, update_status),
            method::RESULT_PUBLISH => call!(ResultPublishParams, result_publish),
            method::TASKS_FETCH => call!(TasksFetchParams, tasks_fetch),
            method::SHUTDOWN => Reply::shutdown_ack(id),
            other => Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method: {other}"),
                ),
            )),
        }
    }
}
