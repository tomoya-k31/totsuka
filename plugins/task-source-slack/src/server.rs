//! JSON-RPC dispatch for the stdio server (F-51). Generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize`'s TokenGuard — is driven in tests with a recorded transport,
//! no network involved.
//!
//! `tasks/fetch` drains the mention pipeline's buffer, `result/publish`
//! presents the agent's reply draft for approval (#107); only
//! `task/update_status` remains a deliberate no-op (Slack has no status
//! column to move).

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, TasksFetchParams, TasksFetchResult,
};
use plugin_protocol::{Capabilities, OutputCapability, RequestId, method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use std::sync::Arc;

use crate::config::{
    LlmConfig, RepoInfo, SlackConfig, default_confidence_threshold, static_config_errors,
};
use crate::error::SlackError;
use crate::llm::ChatTransport;
use crate::pipeline::{self, SharedState};
use crate::slack_api::SlackApi;
use crate::socket_mode::{self, SocketModeOptions};
use crate::transport::{SlackTransport, TransportSettings};

/// Builds the plugin's outbound clients: the Slack Web API transport and the
/// repo-classifier chat transport. Abstracted so the server can be tested
/// with recorded fakes.
pub trait TransportFactory {
    /// The Slack transport this factory produces.
    type Transport: SlackTransport;
    /// The LLM chat transport this factory produces.
    type Chat: ChatTransport;
    /// Build a transport from connection `settings`.
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport;
    /// Build the chat transport for repository classification.
    fn build_chat(&self) -> Self::Chat;
}

/// Connection settings derived from a [`SlackConfig`].
fn settings(config: &SlackConfig) -> TransportSettings<'_> {
    TransportSettings {
        api_url: &config.api_url,
        app_token: &config.app_token,
        user_token: &config.user_token,
        max_retries: config.max_retries,
    }
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

/// The Slack task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    /// Whether `initialize` starts the resident runtime (Socket Mode
    /// connection + mention pipeline). On for production; protocol-level
    /// tests turn it off so canned transports are not consumed by the
    /// background tasks.
    start_runtime: bool,
    /// Set by a successful `initialize` (config parsed + TokenGuard passed).
    session: Option<Session<F::Transport>>,
}

/// An initialized plugin session: the validated config, its Slack client,
/// and the resident runtime.
struct Session<T> {
    config: SlackConfig,
    /// The Web API client `result/publish` presents drafts through (the
    /// running pipeline holds its own Arc clone).
    api: Arc<SlackApi<T>>,
    /// Task buffer + pending-mention index, shared with the pipeline task.
    state: SharedState,
    /// The Socket Mode reader and the mention pipeline (absent when the
    /// runtime is disabled).
    runtime: Vec<tokio::task::AbortHandle>,
}

impl<T> Drop for Session<T> {
    fn drop(&mut self) {
        // A replaced (re-initialize) or ended session must not leak resident
        // tasks that keep reading the socket.
        for handle in &self.runtime {
            handle.abort();
        }
    }
}

impl<F: TransportFactory> Server<F>
where
    F::Transport: 'static,
    F::Chat: 'static,
{
    /// A fresh, uninitialized server using `factory` to build transports.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            start_runtime: true,
            session: None,
        }
    }

    /// A server whose `initialize` does not start the Socket Mode runtime.
    /// For protocol-level tests only.
    pub fn without_runtime(mut self) -> Self {
        self.start_runtime = false;
        self
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
            method::INITIALIZE => self.initialize(id, params).await,
            method::CONFIG_VALIDATE => self.config_validate(id, params),
            method::SHUTDOWN => Reply {
                line: plugin_protocol::jsonrpc::to_line(&Response::result(id, Value::Null)).ok(),
                shutdown: true,
            },
            method::TASKS_FETCH => self.tasks_fetch(id, params),
            method::TASK_UPDATE_STATUS => self.update_status(id, params),
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

    /// `initialize`: deserialize the config, adopt the orchestrator-supplied
    /// repositories when `[[repos]]` is omitted (#109) and the orchestrator's
    /// `[llm]` when the plugin's own is omitted (#119), validate the merged
    /// candidate list, then run the TokenGuard — verify the user token via
    /// `auth.test` (its identity must be `target_user_id`) and the
    /// App-Level Token via `apps.connections.open` — before accepting the
    /// session. A bad token fails startup here, with recovery guidance,
    /// instead of failing later mid-flow.
    async fn initialize(&mut self, id: RequestId, params: Value) -> Reply {
        let init: InitializeParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let mut config: SlackConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid slack plugin config: {e}"),
                    ),
                ));
            }
        };
        // Without an explicit `[[repos]]`, the orchestrator's own
        // `[[repositories]]` (supplied since protocol 0.1.1, #109) become
        // the candidates — one list to maintain instead of two.
        if config.repos.is_empty() {
            config.repos = init
                .repositories
                .into_iter()
                .map(|repo| RepoInfo {
                    name: repo.name,
                    summary: repo.summary,
                    path: repo.path,
                })
                .collect();
        }
        // Without an explicit `[llm]`, the orchestrator's own `[llm]`
        // (supplied since protocol 0.1.2, #119) becomes the classifier
        // default — adopted only when usable as-is (non-empty base_url /
        // model / api_key; the plugin always authenticates its calls), so
        // a keyless core gateway is treated as "nothing supplied" rather
        // than failing the config checks below with a misleading message.
        if config.llm.is_none()
            && let Some(llm) = init.llm
            && !llm.base_url.is_empty()
            && !llm.model.is_empty()
            && let Some(api_key) = llm.api_key.filter(|k| !k.is_empty())
        {
            config.llm = Some(LlmConfig {
                base_url: llm.base_url,
                model: llm.model,
                api_key,
                confidence_threshold: default_confidence_threshold(),
            });
        }
        // The merged candidates + adopted classifier are what the pipeline
        // will actually run on; validate them (and the checks
        // `config/validate` had to defer while they were unknown) before
        // spending network calls on the TokenGuard.
        let mut errors = static_config_errors(&config);
        if config.repos.is_empty() {
            errors.push(
                "no repository candidates → declare `[[repos]]` in plugins/slack.toml or \
                 `[[repositories]]` in the orchestrator's config.toml"
                    .into(),
            );
        }
        if config.repos.len() > 1 && config.llm.is_none() {
            errors.push(
                "`[llm]` is required when more than one repository candidate is declared (it \
                 classifies which repository a mention concerns) → add an `[llm]` table \
                 (base_url / model / api_key) to plugins/slack.toml, or configure the \
                 orchestrator's `[llm]` with an `api_key_ref` in config.toml"
                    .into(),
            );
        }
        if !errors.is_empty() {
            return Reply::respond(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, errors.join("; ")),
            ));
        }
        let api = Arc::new(SlackApi::new(self.factory.build(settings(&config))));
        if let Err(e) = token_guard(&api, &config).await {
            // Credential/identity problems are config-class (fix the token or
            // the config); anything else (network down) is an internal error.
            let code = if e.is_credential() {
                error_code::CONFIG_INVALID
            } else {
                error_code::INTERNAL_ERROR
            };
            return Reply::respond(Response::error(id, Error::new(code, e.to_string())));
        }

        // The resident runtime: Socket Mode reader → mention pipeline →
        // SharedState, which tasks/fetch drains.
        let state = SharedState::default();
        let mut runtime = Vec::new();
        if self.start_runtime {
            let (events, socket) =
                socket_mode::spawn(Arc::clone(&api), SocketModeOptions::default());
            let pipeline = pipeline::spawn(
                Arc::clone(&api),
                Arc::new(self.factory.build_chat()),
                Arc::new(config.clone()),
                events,
                state.clone(),
            );
            runtime.push(socket.abort_handle());
            runtime.push(pipeline.abort_handle());
        }

        self.session = Some(Session {
            config,
            api,
            state,
            runtime,
        });
        Reply::respond(Response::result(id, capabilities_result()))
    }

    /// `config/validate`: schema + static consistency checks only (F-59/F-63).
    /// Deliberately offline — live token verification is `initialize`'s
    /// TokenGuard — so `config validate` / `doctor` probes need no network.
    fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let parsed: ConfigValidateParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let config: SlackConfig = match serde_json::from_value(parsed.config) {
            Ok(c) => c,
            Err(e) => return ok_validate(id, vec![format!("config does not parse: {e}")]),
        };
        ok_validate(id, static_config_errors(&config))
    }

    /// `tasks/fetch`: drain the mention buffer. The trigger condition is not
    /// interpreted in v1 (every buffered mention matches); a second fetch
    /// never sees the same task again.
    fn tasks_fetch(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return not_initialized(id);
        };
        let _parsed: TasksFetchParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let tasks = session.state.drain_tasks();
        tracing::debug!(
            source = session.config.source_name,
            count = tasks.len(),
            "tasks/fetch drained the mention buffer"
        );
        Reply::respond(Response::result(
            id,
            serde_json::to_value(TasksFetchResult { tasks }).unwrap_or(Value::Null),
        ))
    }

    /// `task/update_status`: accepted and ignored — Slack has no status
    /// column to move; the draft lifecycle is driven by the approve/reject
    /// buttons instead.
    fn update_status(&mut self, id: RequestId, params: Value) -> Reply {
        if self.session.is_none() {
            return not_initialized(id);
        }
        let parsed: TaskUpdateStatusParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        tracing::debug!(
            task_id = parsed.task_id,
            status = parsed.status,
            "task/update_status stub: accepted, no source-side status to move"
        );
        Reply::respond(Response::result(id, Value::Null))
    }

    /// `result/publish`: the agent's reply draft arrives here. It becomes a
    /// stored [`Draft`](crate::draft::Draft) presented as an in-thread
    /// ephemeral + a self-DM record, both carrying approve/reject buttons
    /// (#107). Fails only when no draft can be made at all (unknown task
    /// after a restart, empty content); presentation failures are logged and
    /// tolerated.
    async fn result_publish(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return not_initialized(id);
        };
        let parsed: ResultPublishParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match crate::approval::publish_draft(
            session.api.as_ref(),
            &session.config,
            &session.state,
            &parsed.task_id,
            &parsed.content,
        )
        .await
        {
            Ok(()) => Reply::respond(Response::result(id, Value::Null)),
            Err(message) => Reply::respond(Response::error(
                id,
                Error::new(error_code::INTERNAL_ERROR, message),
            )),
        }
    }
}

/// The TokenGuard: `auth.test` must accept the user token, the token's
/// identity must be `target_user_id` (a reply posted with someone else's
/// token would impersonate them), and `apps.connections.open` must accept
/// the App-Level Token. Without the last probe a bad `xapp-` token would
/// only surface inside the background Socket Mode loop — invisible to
/// `initialize`'s caller, so `totsuka doctor` would report the plugin
/// healthy while it can never receive an event.
async fn token_guard<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
) -> Result<(), SlackError> {
    let identity = api.auth_test().await?;
    if identity.user_id != config.target_user_id {
        return Err(SlackError::IdentityMismatch {
            expected: config.target_user_id.clone(),
            actual: identity.user_id,
        });
    }
    api.apps_connections_open().await?;
    Ok(())
}

/// The capabilities this plugin declares (F-33/F-83): a task source that can
/// write results back to the source (`result/publish` → the Slack thread).
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
