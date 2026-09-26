//! The stdio server (F-51): a [`TaskSourceHandler`] whose wire protocol is
//! the SDK's (`plugin_sdk::dispatch::handle_line`, #759). Generic over a
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

use plugin_protocol::Capabilities;
use plugin_protocol::jsonrpc::{Error, error_code};
use plugin_protocol::methods::{
    ClaimedRepo, ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, WorkflowInfo,
};
use plugin_sdk::{
    LineHandler, Reply, SubmitClient, TaskSourceHandler, check_assignee_triggers, not_initialized,
    poll_loop, unknown_exclude_keys, unknown_trigger_keys,
};
use serde_json::Value;

use crate::client::{
    EXCLUDE_KEYS, NotionClient, TRIGGER_KEYS, static_config_errors, unknown_dynamic_refs,
};
use crate::config::NotionConfig;
use crate::transport::{NotionTransport, TransportSettings};

/// The internal fetch cadence when `[notion]` sets no `poll_interval_secs`
/// (F-06's default; the key is this plugin's own since 0.6.0, #554).
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
}

impl<F> TaskSourceHandler for Server<F>
where
    F: TransportFactory + Send,
    F::Transport: Send + Sync + 'static,
{
    /// `initialize`: deserialize the config, build the client, then start the
    /// resident [`poll_loop`] over the supplied triggers — each tick fetches
    /// every trigger and pushes the matching tasks via `task/submit` (0.1.6).
    async fn initialize(&mut self, init: InitializeParams) -> Result<InitializeResult, Error> {
        let mut config: NotionConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Err(Error::new(
                    error_code::CONFIG_INVALID,
                    format!("invalid notion plugin config: {e}"),
                ));
            }
        };
        // The databases come from the Orchestrator's `[[projects]]` and their
        // repositories from `[[repositories]].project` (#554), not `[notion]`.
        config.databases =
            match crate::config::DatabaseConfig::resolve(&init.projects, &init.repositories) {
                Ok(d) => d,
                Err(errors) => {
                    return Err(Error::new(error_code::CONFIG_INVALID, errors.join("; ")));
                }
            };
        // Trigger keys are this plugin's vocabulary, so this is the only
        // place that can tell a typo from a condition (#574). Without it an
        // unread key is dropped and the trigger matches *more* than written.
        let mut config_errors = unknown_trigger_keys(&init.workflows, TRIGGER_KEYS);
        config_errors.extend(unknown_exclude_keys(&init.workflows, EXCLUDE_KEYS));
        // A `@<name>` no `[notion.dynamic.*]` declares must fail here: left
        // alone it goes to Notion verbatim, matches nothing, and ingests zero
        // tasks with no error anywhere (#606).
        config_errors.extend(unknown_dynamic_refs(&init.workflows, &config.dynamic));
        // Both of Notion's assignee prerequisites are optional settings, and
        // both fail silently when missing — an unmapped people property makes
        // every page read as unassigned, and no `notion_user_id` makes `@me`
        // match nobody (#572).
        let (assignee_errors, assignee_warnings) = check_assignee_triggers(
            &init.workflows,
            config.notion_user_id.as_deref(),
            "`notion_user_id`",
            Some(config.property_map.assignee.is_some()),
            "`property_map.assignee`",
            // Notion mints no lane identity for any trigger (#573), so adding a
            // `status` would not make a task repeatable and we do not say it
            // would.
            false,
        );
        config_errors.extend(assignee_errors);
        if !config_errors.is_empty() {
            return Err(Error::new(
                error_code::CONFIG_INVALID,
                config_errors.join("; "),
            ));
        }
        for warning in assignee_warnings {
            tracing::warn!("{warning}");
        }
        let transport = self.factory.build(settings(&config));
        let client = Arc::new(NotionClient::new(config, transport));
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
                    let projects = trigger.projects.clone();
                    async move {
                        client
                            .fetch(&condition, kind.as_deref(), &name, &projects)
                            .await
                            .map_err(|e| e.to_string())
                    }
                },
            ));
            Some(handle.abort_handle())
        };
        let claims = client.config().claimed_repos();
        self.session = Some(Session { client, poll });
        Ok(capabilities_result(claims))
    }

    async fn config_validate(
        &mut self,
        parsed: ConfigValidateParams,
    ) -> Result<ConfigValidateResult, Error> {
        let mut config: NotionConfig = match serde_json::from_value(parsed.config) {
            Ok(c) => c,
            Err(e) => return Ok(validate_result(vec![format!("config does not parse: {e}")])),
        };
        // Same resolution as `initialize` (#554): validating the raw `[notion]`
        // table alone would report "declare at least one database" for every
        // correct config, since the databases are not in it.
        config.databases =
            match crate::config::DatabaseConfig::resolve(&parsed.projects, &parsed.repositories) {
                Ok(d) => d,
                Err(errors) => return Ok(validate_result(errors)),
            };
        let mut errors = static_config_errors(&config);
        // Only ping the API if the config is otherwise well-formed (F-63).
        if errors.is_empty() {
            let transport = self.factory.build(settings(&config));
            let client = NotionClient::new(config, transport);
            if let Err(e) = client.validate().await {
                errors.push(e.to_string());
            } else {
                // Only after the token and the property mapping are known
                // good: otherwise every database would fail the status check
                // with the same underlying fault.
                match client.validate_statuses(&parsed.workflows).await {
                    Ok(status_errors) => errors.extend(status_errors),
                    Err(e) => errors.push(format!(
                        "ステータスの option を検査できなかった: {e} → 検査できていないので、通ったとは読まないこと"
                    )),
                }
            }
        }
        Ok(validate_result(errors))
    }

    async fn update_status(&mut self, parsed: TaskUpdateStatusParams) -> Result<Value, Error> {
        let session = self.session.as_ref().ok_or_else(not_initialized)?;
        session
            .client
            .update_status(&parsed.task_id, &parsed.status)
            .await
            .map(|()| Value::Null)
            .map_err(rpc_error)
    }

    /// Named rather than left to `unknown method`. An older config with
    /// `output = "source"` reaches here only after the agent has done all the
    /// work, and the orchestrator reports whatever comes back as a publish
    /// failure — so the message has to say what to change. `config validate`
    /// catches this earlier, but only when it can see the plugin's declared
    /// outputs.
    async fn result_publish(&mut self, _: ResultPublishParams) -> Result<Value, Error> {
        Err(Error::new(
            error_code::METHOD_NOT_FOUND,
            "`result/publish` was removed: the deliverable is the agent's to write itself. Set the workflow's `profile` to design/implement, or write `output = \"none\"` — `output = \"source\"` no longer has a plugin behind it",
        ))
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
        plugin_sdk::dispatch::handle_line(self, line).await
    }
}

/// The capabilities this plugin declares (F-33/F-83): a task source that
/// publishes nothing — the deliverable is the agent's to write.
///
/// It is a **push** source — it calls `task/submit` and is never polled — but
/// that is no longer declared. Since `tasks/fetch` was removed at protocol
/// 0.2.0 every task source is push-only, so the `task_submit` flag could only
/// ever be `true`; it was removed in 0.5.0 (#496).
fn capabilities_result(claimed_repos: Vec<ClaimedRepo>) -> InitializeResult {
    InitializeResult {
        // No workflow options of its own (#554).
        claimed_options: Vec::new(),
        plugin_version: plugin_version(),
        claimed_repos,
        // No `outputs`: the deliverable is the agent's to write with Notion
        // MCP (#398). Declaring `source` would let a workflow ask this plugin
        // to publish, which it no longer can.
        capabilities: Capabilities::default(),
    }
}

/// This plugin's version, from Cargo. Falls back to `0.0.0` if unparseable.
fn plugin_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(semver::Version::new(0, 0, 0))
}

/// A `config/validate` answer (the RPC itself succeeds; validity is in the
/// payload).
fn validate_result(errors: Vec<String>) -> ConfigValidateResult {
    ConfigValidateResult {
        valid: errors.is_empty(),
        errors,
        warnings: Vec::new(),
    }
}

/// Map a [`crate::error::NotionError`] to a JSON-RPC error carrying its
/// actionable message.
fn rpc_error(error: crate::error::NotionError) -> Error {
    Error::new(error_code::INTERNAL_ERROR, error.to_string())
}
