//! JSON-RPC dispatch for the stdio server (F-51). Generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize` and `config/validate` — is driven in tests with a recorded
//! transport, no network involved.
//!
//! Since protocol 0.1.6 this is a **push source** (`task_submit`): the SDK
//! [`poll_loop`] fetches every `initialize`-supplied trigger on an internal
//! cadence (`poll_interval_secs`, default 60s) and pushes each task via
//! `task/submit` (ADR-0008). `tasks/fetch` no longer exists as of protocol
//! 0.2.0.

use std::sync::Arc;
use std::time::Duration;

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ClaimedRepo, ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    TaskClaimParams, TaskUpdateStatusParams, WorkflowInfo,
};
use plugin_protocol::{Capabilities, RequestId, method};
use plugin_sdk::{
    LineHandler, Reply, SubmitClient, check_assignee_triggers, poll_loop, unknown_trigger_keys,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::{GithubClient, TRIGGER_KEYS, static_config_errors};
use crate::config::GithubConfig;
use crate::transport::GithubTransport;

/// The internal fetch cadence when `[github]` sets no `poll_interval_secs`
/// (F-06's default; the key is this plugin's own since 0.6.0, #554).
const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Builds a transport from resolved connection settings. Abstracted so the
/// server can be tested with a recorded transport.
pub trait TransportFactory {
    /// The transport this factory produces.
    type Transport: GithubTransport;
    /// Build a transport for `endpoint`, authenticating with `token`.
    fn build(&self, endpoint: &str, token: &str, max_retries: u32) -> Self::Transport;
}

/// The GitHub task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    /// The `task/submit` client the poll loop pushes through (0.1.6).
    submit: SubmitClient,
    /// Set by a successful `initialize`.
    session: Option<Session<F::Transport>>,
}

/// An initialized plugin session: the client plus the resident poll loop.
struct Session<T> {
    /// The GraphQL client host-driven methods delegate to (the poll loop
    /// holds its own Arc clone).
    client: Arc<GithubClient<T>>,
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
            method::TASK_CLAIM => self.task_claim(id, params).await,
            // Named rather than left to `unknown method`. An older config with
            // `output = "source"` reaches here only after the agent has done
            // all the work, and the orchestrator reports whatever comes back
            // as a publish failure — so the message has to say what to change.
            // `config validate` catches this earlier, but only when it can see
            // the plugin's declared outputs.
            method::RESULT_PUBLISH => Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    "`result/publish` was removed: the deliverable is the agent's to write itself. Set the workflow's `profile` to design/implement, or write `output = \"none\"` — `output = \"source\"` no longer has a plugin behind it",
                ),
            )),
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
        let mut config: GithubConfig = match serde_json::from_value(init.config) {
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
        // The boards come from the Orchestrator's `[[projects]]` and their
        // repositories from `[[repositories]].project` (#554), not from
        // `[github]`.
        config.projects =
            match crate::config::ProjectConfig::resolve(&init.projects, &init.repositories) {
                Ok(p) => p,
                Err(errors) => {
                    return Reply::respond(Response::error(
                        id,
                        Error::new(error_code::CONFIG_INVALID, errors.join("; ")),
                    ));
                }
            };
        // Trigger keys are this plugin's vocabulary, so this is the only
        // place that can tell a typo from a condition (#574). Without it an
        // unread key is dropped and the trigger matches *more* than written.
        let mut config_errors = unknown_trigger_keys(&init.workflows, TRIGGER_KEYS);
        // `github_login` is required, so `@me` always has something to compare
        // against; Issue assignees are built in, so there is no property to map
        // (#572).
        let (assignee_errors, assignee_warnings) = check_assignee_triggers(
            &init.workflows,
            Some(config.github_login.as_str()),
            "`github_login`",
            None,
            "",
            // GitHub keys deliveries on the status cell, so a column move repeats
            // the task and "add a `status`" is real advice here (#556).
            true,
        );
        config_errors.extend(assignee_errors);
        if !config_errors.is_empty() {
            return Reply::respond(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, config_errors.join("; ")),
            ));
        }
        for warning in assignee_warnings {
            tracing::warn!("{warning}");
        }
        let transport = self
            .factory
            .build(&config.api_url, &config.token, config.max_retries);
        let client = Arc::new(GithubClient::new(config, transport));
        let poll = if init.workflows.is_empty() {
            None
        } else {
            // 0 would make the loop spin without sleeping (API hammering);
            // fall back to the default rather than honoring it.
            let secs = match client.config().poll_interval_secs {
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
                init.workflows,
                interval,
                self.submit.clone(),
                move |trigger: &WorkflowInfo| {
                    let client = Arc::clone(&fetch_client);
                    let condition = trigger.trigger.clone();
                    let kind = trigger.instructions_kind.clone();
                    let name = trigger.workflow.clone();
                    async move {
                        client
                            .fetch(&condition, kind.as_deref(), &name)
                            .await
                            .map_err(|e| e.to_string())
                    }
                },
            ));
            Some(handle.abort_handle())
        };
        let claims = client.config().claimed_repos();
        self.session = Some(Session { client, poll });
        Reply::respond(Response::result(id, capabilities_result(claims)))
    }

    async fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let parsed: ConfigValidateParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        let mut config: GithubConfig = match serde_json::from_value(parsed.config) {
            Ok(c) => c,
            Err(e) => return ok_validate(id, vec![format!("config does not parse: {e}")]),
        };
        // Same resolution as `initialize` (#554): validating the raw `[github]`
        // table alone would report "declare at least one board" for every
        // correct config, since the boards are not in it.
        config.projects =
            match crate::config::ProjectConfig::resolve(&parsed.projects, &parsed.repositories) {
                Ok(p) => p,
                Err(errors) => return ok_validate(id, errors),
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

    /// `task/claim` (#556): delegate to
    /// [`GithubClient::claim`](crate::client::GithubClient::claim). Errors —
    /// including "the adjudication cannot be decided on this read" — go back
    /// as JSON-RPC errors, which the Orchestrator answers by leaving the task
    /// queued and retrying next cycle.
    async fn task_claim(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return not_initialized(id);
        };
        let parsed: TaskClaimParams = match parse_params(&params) {
            Ok(v) => v,
            Err(reply) => return reply.with_id(id),
        };
        match session.client.claim(&parsed.task_id).await {
            Ok(result) => Reply::respond(Response::result(
                id,
                serde_json::to_value(result).unwrap_or(Value::Null),
            )),
            Err(e) => Reply::respond(rpc_error(id, &e)),
        }
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

/// The capabilities this plugin declares (F-33/F-83): a task source that
/// publishes nothing — the deliverable is the agent's to write.
///
/// It is a **push** source — it calls `task/submit` and is never polled — but
/// that is no longer declared. Since `tasks/fetch` was removed at protocol
/// 0.2.0 every task source is push-only, so the `task_submit` flag could only
/// ever be `true`; it was removed in 0.5.0 (#496).
fn capabilities_result(claimed_repos: Vec<ClaimedRepo>) -> Value {
    let result = InitializeResult {
        // No workflow options of its own (#554).
        claimed_options: Vec::new(),
        plugin_version: plugin_version(),
        claimed_repos,
        // No `outputs`: the deliverable is the agent's to write with `gh`
        // (#398). Declaring `source` would let a workflow ask this plugin to
        // publish, which it no longer can. `task_claim` (#556): this plugin
        // answers `task/claim` by self-assigning the issue — the one place it
        // writes to an Issue rather than a Project.
        capabilities: Capabilities {
            task_claim: true,
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
