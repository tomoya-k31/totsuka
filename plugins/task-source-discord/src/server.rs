//! JSON-RPC dispatch for the stdio server, generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize`'s token guard — is driven in tests with no network.

use std::sync::Arc;

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams,
};
use plugin_protocol::{Capabilities, OutputCapability, RequestId, method};
use plugin_sdk::{LineHandler, Reply, SubmitClient, request_id, unknown_trigger_keys};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{DiscordConfig, static_config_errors};
use crate::discord_api::DiscordApi;
use crate::pipeline::{self, SharedState};
use crate::transport::{DiscordTransport, TransportSettings};
use crate::watch::WatchTriggers;

/// The `[[workflows]].trigger` keys this source reads.
///
/// This source has exactly one trigger kind, so anything else is a typo —
/// and a dropped key would widen the trigger rather than narrow it, which is
/// why an unknown one fails startup instead of being ignored.
const TRIGGER_KEYS: &[&str] = &["channel", "channel_name", "repo", "from"];

/// Discord's ceiling on `GET /channels/{id}/messages?limit=`.
const DISCORD_MESSAGE_PAGE_MAX: u32 = 100;

/// Builds the plugin's outbound transport. Abstracted so the server is tested
/// against recorded responses.
pub trait TransportFactory {
    /// The transport this factory produces.
    type Transport: DiscordTransport;
    /// Build a transport from connection `settings`.
    fn build(&self, settings: TransportSettings<'_>) -> Self::Transport;
}

/// An initialized session: validated config, its client, and the runtime.
struct Session<T> {
    api: Arc<DiscordApi<T>>,
    state: SharedState,
    runtime: Vec<tokio::task::AbortHandle>,
}

impl<T> Drop for Session<T> {
    fn drop(&mut self) {
        // A replaced or ended session must not leak a task still reading the
        // Gateway.
        for handle in &self.runtime {
            handle.abort();
        }
    }
}

/// The Discord task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    submit: SubmitClient,
    start_runtime: bool,
    session: Option<Session<F::Transport>>,
}

impl<F: TransportFactory> Server<F>
where
    F::Transport: Send + Sync + 'static,
{
    /// A fresh, uninitialized server.
    pub fn new(factory: F, submit: SubmitClient) -> Self {
        Self {
            factory,
            submit,
            start_runtime: true,
            session: None,
        }
    }

    /// Disable the resident runtime — protocol-level tests drive `initialize`
    /// without a Gateway consuming their canned responses.
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
        let Ok(request) = serde_json::from_str::<Value>(trimmed) else {
            // A null id, not an empty string: the id is *unknown*, and saying
            // `""` would claim a correlation key the caller never sent.
            return Reply::respond(Response::error_without_id(Error::new(
                error_code::PARSE_ERROR,
                "request was not valid JSON",
            )));
        };
        // A message without an `id` is a notification: never answered.
        let Some(id) = request.get("id").map(request_id) else {
            if request.get("method").and_then(Value::as_str) == Some(method::SHUTDOWN) {
                self.session = None;
            }
            return Reply::none();
        };
        let Some(method_name) = request.get("method").and_then(Value::as_str) else {
            return Reply::respond(Response::error(
                id,
                Error::new(error_code::INVALID_REQUEST, "missing `method`"),
            ));
        };
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        self.dispatch(id, method_name, params).await
    }

    async fn dispatch(&mut self, id: RequestId, method_name: &str, params: Value) -> Reply {
        match method_name {
            method::INITIALIZE => self.initialize(id, params).await,
            method::CONFIG_VALIDATE => self.config_validate(id, params),
            method::TASK_UPDATE_STATUS => self.update_status(id, params),
            method::RESULT_PUBLISH => self.result_publish(id, params).await,
            method::SHUTDOWN => {
                // `shutdown_ack`, not `respond`: the serve loop exits on the
                // reply's `shutdown` flag, so a plain response acks the
                // request and then keeps serving.
                self.session = None;
                Reply::shutdown_ack(id)
            }
            other => Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::METHOD_NOT_FOUND,
                    format!("unknown method `{other}`"),
                ),
            )),
        }
    }

    async fn initialize(&mut self, id: RequestId, params: Value) -> Reply {
        let init: InitializeParams = match parse_params(&params) {
            Ok(value) => value,
            Err(message) => {
                // `INVALID_PARAMS`, not `CONFIG_INVALID`: a request this
                // plugin cannot deserialize is a protocol problem, and
                // reporting it as a config one sends the operator to edit a
                // file that is not the cause.
                return Reply::respond(Response::error(
                    id,
                    Error::new(error_code::INVALID_PARAMS, message),
                ));
            }
        };
        let config: DiscordConfig = match serde_json::from_value(init.config) {
            Ok(config) => config,
            Err(e) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(
                        error_code::CONFIG_INVALID,
                        format!("invalid discord plugin config: {e}"),
                    ),
                ));
            }
        };

        let mut errors = static_config_errors(&config);
        errors.extend(unknown_trigger_keys(&init.workflows, TRIGGER_KEYS));

        let repo_names: Vec<&str> = init.repositories.iter().map(|r| r.name.as_str()).collect();
        let triggers = match plugin_sdk::resolve_watch_triggers(
            &init.workflows,
            &repo_names,
            Some(&config.operator_user_id),
            "operator_user_id",
        ) {
            Ok(triggers) => triggers,
            Err(mut e) => {
                errors.append(&mut e);
                Vec::new()
            }
        };
        // A source that watches nothing has nothing to do, and starting it
        // that way is always a mistake rather than a choice — this plugin has
        // no other trigger kind to fall back on.
        if triggers.is_empty() && errors.is_empty() {
            errors.push(
                "no workflow watches a Discord channel → give a `[[workflows]]` entry with \
                 source = \"discord\" a `trigger = { channel = \"…\", channel_name = \"…\", \
                 repo = \"…\" }`, or disable this plugin"
                    .into(),
            );
        }
        // Discord caps `limit` on the messages route at 100. A larger value is
        // refused with a 400 on **every** backfill, which surfaces as one warn
        // line and then a recovery path that silently does nothing — so it is
        // refused here, where the number is still visible to the operator.
        if config
            .watch_backfill_limit
            .is_some_and(|n| n > DISCORD_MESSAGE_PAGE_MAX)
        {
            errors.push(format!(
                "`watch_backfill_limit` is {}, but Discord returns at most \
                 {DISCORD_MESSAGE_PAGE_MAX} messages per request → lower it, or leave it out \
                 for the default",
                config.watch_backfill_limit.unwrap_or_default()
            ));
        }
        let backfill_limits = match plugin_sdk::BackfillLimits::new(
            config.watch_backfill_limit,
            config.watch_backfill_max_age_hours,
        ) {
            Ok(limits) => limits,
            Err(e) => {
                errors.push(e);
                plugin_sdk::BackfillLimits::default()
            }
        };
        if !errors.is_empty() {
            return Reply::respond(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, errors.join("; ")),
            ));
        }

        let api = Arc::new(DiscordApi::new(self.factory.build(TransportSettings {
            api_url: &config.api_url,
            bot_token: &config.bot_token,
            max_retries: config.max_retries,
        })));
        // The token guard: a wrong or revoked token must stop the plugin here,
        // with guidance, rather than surface later as a Gateway that will not
        // stay connected.
        let self_id = match api.current_user_id().await {
            Ok(id) => id,
            Err(e) => {
                let code = if e.is_credential() {
                    error_code::CONFIG_INVALID
                } else {
                    error_code::INTERNAL_ERROR
                };
                return Reply::respond(Response::error(id, Error::new(code, e.to_string())));
            }
        };

        let triggers = WatchTriggers::new(
            triggers,
            &init.workflows,
            &config.operator_user_id,
            &self_id,
        );
        let state = SharedState::default();
        let mut runtime = Vec::new();
        if self.start_runtime {
            let handle = crate::run::spawn(
                Arc::clone(&api),
                Arc::new(config),
                triggers,
                backfill_limits,
                state.clone(),
                self.submit.clone(),
            );
            runtime.push(handle.abort_handle());
        }

        self.session = Some(Session {
            api,
            state,
            runtime,
        });
        let result = InitializeResult {
            plugin_version: plugin_version(),
            capabilities: Capabilities {
                outputs: vec![OutputCapability::Source],
                ..Default::default()
            },
            claimed_repos: Vec::new(),
            // This source reads no `[[workflows]]` keys of its own: everything
            // it needs is inside `trigger`, which the Orchestrator never
            // claims. Claiming a key it ignored would turn a typo into
            // silence, which is the failure the handshake exists to remove.
            claimed_options: Vec::new(),
        };
        Reply::respond(Response::result(
            id,
            serde_json::to_value(result).unwrap_or(Value::Null),
        ))
    }

    /// Schema + static checks only. Deliberately offline: live token
    /// verification is `initialize`'s job, so `config validate` and `doctor`
    /// probes need no network.
    fn config_validate(&mut self, id: RequestId, params: Value) -> Reply {
        let parsed: ConfigValidateParams = match parse_params(&params) {
            Ok(value) => value,
            Err(message) => {
                return Reply::respond(Response::result(
                    id,
                    serde_json::to_value(ConfigValidateResult {
                        valid: false,
                        errors: vec![message],
                    })
                    .unwrap_or(Value::Null),
                ));
            }
        };
        let errors = match serde_json::from_value::<DiscordConfig>(parsed.config) {
            Ok(config) => static_config_errors(&config),
            Err(e) => vec![format!("invalid discord plugin config: {e}")],
        };
        Reply::respond(Response::result(
            id,
            serde_json::to_value(ConfigValidateResult {
                valid: errors.is_empty(),
                errors,
            })
            .unwrap_or(Value::Null),
        ))
    }

    /// A deliberate no-op: a Discord post has no status column to move.
    fn update_status(&mut self, id: RequestId, params: Value) -> Reply {
        match parse_params::<TaskUpdateStatusParams>(&params) {
            Ok(_) => Reply::respond(Response::result(id, Value::Null)),
            Err(message) => Reply::respond(Response::error(
                id,
                Error::new(error_code::INVALID_PARAMS, message),
            )),
        }
    }

    async fn result_publish(&mut self, id: RequestId, params: Value) -> Reply {
        let Some(session) = self.session.as_ref() else {
            return Reply::respond(Response::error(
                id,
                Error::new(
                    error_code::INVALID_REQUEST,
                    "plugin not initialized → send `initialize` first",
                ),
            ));
        };
        let parsed: ResultPublishParams = match parse_params(&params) {
            Ok(value) => value,
            Err(message) => {
                return Reply::respond(Response::error(
                    id,
                    Error::new(error_code::INVALID_PARAMS, message),
                ));
            }
        };
        match pipeline::publish_result(
            session.api.as_ref(),
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

impl<F> LineHandler for Server<F>
where
    F: TransportFactory + Send,
    F::Transport: Send + Sync + 'static,
{
    async fn handle_line(&mut self, line: &str) -> Reply {
        Server::handle_line(self, line).await
    }
}

/// This plugin's own version, from the crate metadata.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver")
}

fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, String> {
    serde_json::from_value(params.clone()).map_err(|e| format!("invalid params: {e}"))
}
