//! JSON-RPC dispatch boilerplate shared by every plugin kind: the [`Reply`]
//! shape, id/params helpers, and a typed [`TaskSourceHandler`] whose
//! [`handle_line`] (or the [`TaskSourceServer`] wrapper) implements the full
//! line protocol (parse errors, notifications, `shutdown`, unknown methods).
//! The agent_ide counterpart is [`crate::agent_ide`].

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskClaimParams, TaskClaimResult, TaskUpdateStatusParams,
};
use plugin_protocol::{RequestId, method};
use serde::Serialize;
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

/// Convert a JSON id value into a [`RequestId`]. Non-string scalars (e.g. a
/// float) fall back to their JSON rendering so correlation stays possible —
/// the same convention as the in-repo plugin servers.
pub fn request_id(id: &Value) -> RequestId {
    if let Some(n) = id.as_i64() {
        RequestId::Number(n)
    } else if let Some(s) = id.as_str() {
        RequestId::Str(s.to_string())
    } else {
        RequestId::Str(id.to_string())
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

    /// `task/claim` (0.6.1, #556): claim a task for exclusive execution.
    ///
    /// Defaulted to a `METHOD_NOT_FOUND` error so existing handlers keep
    /// compiling — the Orchestrator only calls this on plugins whose
    /// `initialize` declared the `task_claim` capability, so a handler that
    /// overrides this must declare the flag, and one that declares the flag
    /// must override this.
    fn task_claim(
        &mut self,
        params: TaskClaimParams,
    ) -> impl Future<Output = Result<TaskClaimResult, Error>> + Send {
        let _ = params;
        async {
            Err(Error::new(
                error_code::METHOD_NOT_FOUND,
                "task/claim is not supported by this plugin → do not declare the `task_claim` capability",
            ))
        }
    }
}

/// The error for a method that needs `initialize` first — the same code and
/// message for every plugin kind.
pub fn not_initialized() -> Error {
    Error::new(
        error_code::INVALID_REQUEST,
        "plugin not initialized → send `initialize` first",
    )
}

/// One request, pulled out of a line: its id, method and params.
pub(crate) struct Request {
    pub(crate) id: RequestId,
    pub(crate) method: String,
    pub(crate) params: Value,
}

/// Parse one NDJSON line into a [`Request`], or the [`Reply`] it gets
/// without reaching a handler: nothing for a blank line or a notification
/// (no `id`: never answered), `PARSE_ERROR` with a null id for non-JSON.
pub(crate) fn parse_request(line: &str) -> Result<Request, Reply> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(Reply::none());
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Err(Reply::respond(Response::error_without_id(Error::new(
            error_code::PARSE_ERROR,
            "request was not valid JSON",
        ))));
    };
    let Some(id) = value.get("id").map(request_id) else {
        return Err(Reply::none());
    };
    Ok(Request {
        id,
        method: value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        params: value.get("params").cloned().unwrap_or(Value::Null),
    })
}

/// Encode a handler's outcome as the response to `id`.
pub(crate) fn respond<T: Serialize>(id: RequestId, outcome: Result<T, Error>) -> Reply {
    Reply::respond(match outcome {
        Ok(result) => match serde_json::to_value(result) {
            Ok(v) => Response::result(id, v),
            Err(e) => Response::error(
                id,
                Error::new(
                    error_code::INTERNAL_ERROR,
                    format!("failed to encode result: {e}"),
                ),
            ),
        },
        Err(error) => Response::error(id, error),
    })
}

/// `METHOD_NOT_FOUND` for a method this plugin kind does not serve.
pub(crate) fn unknown_method(id: RequestId, method: &str) -> Reply {
    Reply::respond(Response::error(
        id,
        Error::new(
            error_code::METHOD_NOT_FOUND,
            format!("unknown method: {method}"),
        ),
    ))
}

/// Handle one NDJSON line against `handler`: the whole task_source wire
/// protocol. A plugin whose server *is* the handler implements
/// [`LineHandler`] by calling this; [`TaskSourceServer`] does the same for a
/// handler it owns.
pub async fn handle_line<H: TaskSourceHandler>(handler: &mut H, line: &str) -> Reply {
    let Request { id, method, params } = match parse_request(line) {
        Ok(request) => request,
        Err(reply) => return reply,
    };
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
        method::TASK_UPDATE_STATUS => call!(TaskUpdateStatusParams, update_status),
        method::TASK_CLAIM => call!(TaskClaimParams, task_claim),
        method::RESULT_PUBLISH => call!(ResultPublishParams, result_publish),
        method::SHUTDOWN => Reply::shutdown_ack(id),
        other => unknown_method(id, other),
    }
}

/// Adapter: drive a [`TaskSourceHandler`] as a [`LineHandler`].
pub struct TaskSourceServer<H>(pub H);

impl<H: TaskSourceHandler> LineHandler for TaskSourceServer<H> {
    async fn handle_line(&mut self, line: &str) -> Reply {
        handle_line(&mut self.0, line).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_protocol::methods::TaskClaimParams;

    /// A handler that overrides nothing, so `task_claim` is the default.
    struct Bare;
    impl TaskSourceHandler for Bare {
        async fn initialize(&mut self, _: InitializeParams) -> Result<InitializeResult, Error> {
            unreachable!("not exercised")
        }
        async fn config_validate(
            &mut self,
            _: ConfigValidateParams,
        ) -> Result<ConfigValidateResult, Error> {
            unreachable!("not exercised")
        }
        async fn update_status(&mut self, _: TaskUpdateStatusParams) -> Result<Value, Error> {
            unreachable!("not exercised")
        }
        async fn result_publish(&mut self, _: ResultPublishParams) -> Result<Value, Error> {
            unreachable!("not exercised")
        }
    }

    /// The default `task_claim` refuses with `METHOD_NOT_FOUND` — and the
    /// message, read by a plugin author, must not carry source indentation.
    /// A wrapped string literal that loses its line continuations keeps the
    /// indentation *inside* the message and `contains` alone never notices;
    /// this shipped twice already (#491, and the first cut of this very
    /// method), so the guard is part of the contract now.
    #[tokio::test]
    async fn default_task_claim_refuses_with_a_clean_message() {
        let err = Bare
            .task_claim(TaskClaimParams {
                task_id: "x".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, error_code::METHOD_NOT_FOUND);
        assert!(
            !err.message.contains("  "),
            "the message carries source indentation: {:?}",
            err.message
        );
        assert!(err.message.contains("task_claim"), "{}", err.message);
    }
}
