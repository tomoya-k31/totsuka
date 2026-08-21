//! JSON-RPC dispatch for the stdio server (F-51). Generic over a
//! [`TransportFactory`] so the whole request/response surface — including
//! `initialize`'s TokenGuard — is driven in tests with a recorded transport,
//! no network involved.
//!
//! This is a **push source** (`task_submit`, since protocol 0.1.6): the
//! mention pipeline submits tasks via the SDK [`SubmitClient`].
//! `result/publish` presents the agent's reply draft for approval (#107);
//! only `task/update_status` remains a deliberate no-op (Slack has no status
//! column to move). `tasks/fetch` no longer exists as of protocol 0.2.0.

use plugin_protocol::jsonrpc::{Error, Response, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams,
};
use plugin_protocol::{Capabilities, OutputCapability, RequestId, method};
use plugin_sdk::{LineHandler, LookupClient, Reply, SubmitClient};
use serde::de::DeserializeOwned;
use serde_json::Value;

use std::sync::Arc;

use crate::config::{
    LlmConfig, RepoInfo, SlackConfig, default_confidence_threshold, static_config_errors,
};
use crate::draft::DraftStore;
use crate::error::SlackError;
use crate::llm::ChatTransport;
use crate::persist;
use crate::pipeline::{self, SharedState};
use crate::reaction::{ReactionTriggers, WorkflowTrigger};
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

/// `(workflow name, its `trigger.reaction`)` for every trigger the
/// Orchestrator sent, in definition order (#396).
///
/// The trigger is an opaque `serde_json::Value` (a TOML inline table converted
/// to JSON), so a non-string `reaction` cannot be turned into an emoji here and
/// reads as absent.
///
/// **A present-but-unreadable value is warned about rather than ignored.** A
/// current Orchestrator rejects it (`validate_workflows`), so this only fires
/// against an older core — and there the same value is *also* skipped by
/// `Trigger::matches`, which makes that workflow match every task from this
/// source. The two halves then fail in opposite directions from one typo: no
/// emoji is registered here, and everything is swallowed there. Neither end
/// reports an error on its own, so the warning is the only signal.
fn workflow_reactions(triggers: &[plugin_protocol::methods::TriggerInfo]) -> Vec<WorkflowTrigger> {
    triggers
        .iter()
        .map(|t| {
            let raw = t.trigger.get("reaction");
            if let Some(value) = raw
                && value.as_str().is_none()
            {
                tracing::warn!(
                    workflow = %t.workflow,
                    value = %value,
                    "`trigger.reaction` is not a string → this workflow registers no emoji, and \
                     an orchestrator that does not reject the value will match every task from \
                     this source against it; write the emoji name as a string \
                     (`reaction = \"eyes\"`)"
                );
            }
            WorkflowTrigger {
                workflow: t.workflow.clone(),
                reaction: raw.and_then(Value::as_str).map(str::to_string),
                // Absent from an older Orchestrator → no prefix → the
                // conversation id, which is the pre-#397 behaviour.
                task_id_prefix: t
                    .trigger
                    .get("task_id_prefix")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                // Which instruction set the workflow's profile wants (#398).
                // Absent only from a core older than #404 — which predates
                // `task_id_prefix` (#405), so "a prefix but no kind" is not a
                // combination any shipped core produces. Such a core sends
                // neither, and the pipeline's fallback is the reply draft,
                // exactly what it always produced.
                instructions_kind: t
                    .trigger
                    .get("instructions_kind")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }
        })
        .collect()
}

/// Connection settings derived from a [`SlackConfig`].
fn settings(config: &SlackConfig) -> TransportSettings<'_> {
    TransportSettings {
        api_url: &config.api_url,
        app_token: &config.app_token,
        user_token: &config.user_token,
        bot_token: config.bot_token.as_deref(),
        max_retries: config.max_retries,
    }
}

/// The Slack task-source stdio server.
pub struct Server<F: TransportFactory> {
    factory: F,
    /// The `task/submit` client the mention pipeline pushes through (0.1.6).
    submit: SubmitClient,
    /// The `task/lookup` client the pipeline asks before resolving a
    /// repository (0.2.4, #242).
    lookup: LookupClient,
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
    /// A fresh, uninitialized server using `factory` to build transports,
    /// `submit` to push tasks (0.1.6), and `lookup` to ask whether a
    /// conversation is already known before resolving one (0.2.4).
    pub fn new(factory: F, submit: SubmitClient, lookup: LookupClient) -> Self {
        Self {
            factory,
            submit,
            lookup,
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
    /// `auth.test` (its identity must be `target_user_id`), the
    /// App-Level Token via `apps.connections.open`, and, when configured,
    /// the bot token via a bot-authenticated `auth.test` (#305) — before
    /// accepting the session. A bad token fails startup here, with recovery
    /// guidance, instead of failing later mid-flow.
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
        // Prompt overrides are advisory-checked here (#318). This could be a
        // hard error — `config_validate` below exists — but it deliberately is
        // not: an unknown placeholder renders verbatim, so the symptom is a
        // visible `{token}` in a draft, and every prompt here is LLM-facing.
        // Core's `[prompts]` errors on the same typo class because there the
        // deleted text is the completion-marker convention.
        for (key, placeholder) in config.prompts.unknown_placeholders() {
            tracing::warn!(
                key,
                placeholder,
                "slack prompt override references an unknown placeholder → it will be rendered literally; check the key's allowed placeholders in docs/config-reference.md"
            );
        }
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
        // Reaction triggers arrive on this call as `[[workflows]].trigger.reaction`
        // (#396), so this is where they are resolved.
        let reaction_triggers = match ReactionTriggers::resolve(&workflow_reactions(&init.triggers))
        {
            Ok(t) => t,
            Err(mut e) => {
                errors.append(&mut e);
                ReactionTriggers::default()
            }
        };
        if !errors.is_empty() {
            return Reply::respond(Response::error(
                id,
                Error::new(error_code::CONFIG_INVALID, errors.join("; ")),
            ));
        }
        let api = Arc::new(SlackApi::new(self.factory.build(settings(&config))));
        if let Err(e) = token_guard(&api, &config, &reaction_triggers).await {
            // Credential/identity problems are config-class (fix the token or
            // the config); anything else (network down) is an internal error.
            let code = if e.is_credential() {
                error_code::CONFIG_INVALID
            } else {
                error_code::INTERNAL_ERROR
            };
            return Reply::respond(Response::error(id, Error::new(code, e.to_string())));
        }

        // The draft store persists across restarts (#122) so approval
        // buttons survive a `run --watch` restart. When no state directory
        // can be resolved (HOME/XDG_STATE_HOME unset), degrade to the
        // in-memory store instead of failing startup.
        let drafts = match persist::drafts_path(config.state_dir.as_deref(), &config.source_name) {
            Some(path) => DraftStore::load(path),
            None => {
                // Unresolvable state dir (HOME/XDG_STATE_HOME unset) or a
                // source_name that is not a plain directory name — the
                // specific cause was already logged by `drafts_path`.
                tracing::warn!(
                    "no persistable draft-store path; drafts are in-memory only and their \
                     buttons will not survive a restart"
                );
                DraftStore::default()
            }
        };
        // The resident runtime: Socket Mode reader → mention pipeline →
        // `task/submit` push (0.1.6).
        let state = SharedState::new(drafts);
        let mut runtime = Vec::new();
        if self.start_runtime {
            let (events, socket) =
                socket_mode::spawn(Arc::clone(&api), SocketModeOptions::default());
            let pipeline = pipeline::spawn(
                Arc::clone(&api),
                Arc::new(self.factory.build_chat()),
                Arc::new(config.clone()),
                reaction_triggers,
                events,
                state.clone(),
                self.submit.clone(),
                self.lookup.clone(),
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

/// Drive the server from the SDK stdio runtime (`plugin_sdk::serve`), which
/// also routes `task/submit` acks back to the shared [`SubmitClient`].
impl<F> LineHandler for Server<F>
where
    F: TransportFactory + Send,
    F::Transport: Send + Sync + 'static,
    F::Chat: Send + Sync + 'static,
{
    async fn handle_line(&mut self, line: &str) -> Reply {
        Server::handle_line(self, line).await
    }
}

/// The TokenGuard: `auth.test` must accept the user token, the token's
/// identity must be `target_user_id` (a reply posted with someone else's
/// token would impersonate them), `apps.connections.open` must accept
/// the App-Level Token, and — only when a `bot_token` is configured — a
/// bot-authenticated `auth.test` must accept it too (#305). Without the
/// xapp probe a bad `xapp-` token would only surface inside the background
/// Socket Mode loop — invisible to `initialize`'s caller, so `totsuka
/// doctor` would report the plugin healthy while it can never receive an
/// event.
async fn token_guard<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    reactions: &ReactionTriggers,
) -> Result<(), SlackError> {
    let identity = api.auth_test().await?;
    if identity.user_id != config.target_user_id {
        return Err(SlackError::IdentityMismatch {
            expected: config.target_user_id.clone(),
            actual: identity.user_id,
        });
    }
    api.apps_connections_open().await?;
    // An explicitly configured `bot_token` gets the same treatment as the
    // xapp token: probe it here so a dead one fails startup with guidance
    // (visible to `doctor`) instead of silently dropping every nudge (#305).
    // Absent = nudges off by choice; nothing to probe.
    if config.bot_token.is_some() {
        api.auth_test_bot().await?;
    }
    check_scopes(api, config, reactions).await;
    Ok(())
}

/// Warn when the user token lacks a scope the config depends on (#379).
///
/// **A missing scope is silent.** Slack simply does not deliver the events it
/// gates, and reports nothing: a reaction trigger set against a token without
/// `reactions:read` produces no `reaction_added`, no error, and no log line —
/// the feature is configured, `doctor` is green, and nothing happens.
/// That cost hours to diagnose live, which is why the check exists at all.
///
/// **Warn rather than fail.** The plugin still does its main job (mentions,
/// drafts, approvals) with the scope missing; only the opt-in feature is dead.
/// Refusing to start would take a working setup down over a feature the
/// operator may not even be relying on yet.
///
/// **An unknown scope set says nothing.** `granted_scopes` answers `None` on
/// any transport that cannot read headers, and that must stay silent — a check
/// that cannot see is not a check that found a problem.
async fn check_scopes<T: SlackTransport>(
    api: &SlackApi<T>,
    config: &SlackConfig,
    reactions: &ReactionTriggers,
) {
    let scopes = match api.granted_scopes().await {
        Ok(Some(scopes)) => scopes,
        // Unreadable or unsupported: say nothing rather than guess.
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(error = %e, "could not read the token's scopes; skipping the scope check");
            return;
        }
    };
    for warning in scope_warnings(&scopes, config, reactions) {
        tracing::warn!("{warning}");
    }
}

/// The scope problems `config` has against `scopes` — one message each, empty
/// when there is nothing to say.
///
/// Split out from [`check_scopes`] so the *decision* can be tested directly.
/// Asserting "initialize still succeeded" would pass just as well with the
/// check deleted, which is no test at all.
fn scope_warnings(
    scopes: &[String],
    config: &SlackConfig,
    reactions: &ReactionTriggers,
) -> Vec<String> {
    let has = |scope: &str| scopes.iter().any(|s| s == scope);
    let mut warnings = Vec::new();

    // Keyed off the **resolved** trigger set (#396) — a missing scope is
    // invisible without this check: Slack delivers no event and reports no
    // error, so the config looks right and nothing happens.
    if !reactions.is_empty() && !has("reactions:read") {
        warnings.push(
            concat!(
                "a reaction trigger is configured but the user token has no ",
                "`reactions:read` scope → Slack will not deliver `reaction_added` at all, ",
                "so reaction triggers are silently dead. Update the app with the current ",
                "manifest, Reinstall to Workspace, then store the NEW `xoxp-` and `xoxb-` ",
                "tokens (a reinstall reissues both).",
            )
            .to_string(),
        );
    }
    // Either scope resolves a channel name; private-only or public-only setups
    // are both legitimate, so this fires only when neither is present.
    if !config.channel_groups.is_empty() && !(has("channels:read") || has("groups:read")) {
        warnings.push(
            concat!(
                "`[[channel_groups]]` is set but the user token has neither ",
                "`channels:read` nor `groups:read` → channel names cannot be resolved, ",
                "so every prefix rule misses and repository selection always falls ",
                "through to the LLM or the picker. Reinstall the app with the current ",
                "manifest and update both tokens.",
            )
            .to_string(),
        );
    }
    warnings
}

/// The capabilities this plugin declares (F-33/F-83): a task source that can
/// write results back to the source (`result/publish` → the Slack thread).
///
/// It is a **push** source — it calls `task/submit` and is never polled — but
/// that is no longer declared. Since `tasks/fetch` was removed at protocol
/// 0.2.0 every task source is push-only, so the `task_submit` flag could only
/// ever be `true`; it was removed in 0.5.0 (#496).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(channel_group: bool) -> SlackConfig {
        let mut value = serde_json::json!({
            "app_token": "xapp-1-A1-t",
            "user_token": "xoxp-t",
            "target_user_id": "U_ME",
        });
        if channel_group {
            value["repos"] = serde_json::json!([{ "name": "web-app" }]);
            value["channel_groups"] =
                serde_json::json!([{ "prefix": "dev-", "repos": ["web-app"] }]);
        }
        serde_json::from_value(value).expect("a valid config")
    }

    fn owned(scopes: &[&str]) -> Vec<String> {
        scopes.iter().map(|s| s.to_string()).collect()
    }

    /// A resolved trigger set holding `names`, as workflow triggers.
    fn triggers(names: &[&str]) -> ReactionTriggers {
        let workflows: Vec<WorkflowTrigger> = names
            .iter()
            .map(|n| WorkflowTrigger {
                workflow: format!("wf-{n}"),
                reaction: Some((*n).to_string()),
                task_id_prefix: None,
                instructions_kind: None,
            })
            .collect();
        ReactionTriggers::resolve(&workflows).expect("valid")
    }

    /// The case that cost hours live (#379): the feature is configured, the
    /// token cannot receive its events, and Slack reports nothing.
    ///
    /// Keyed off the **resolved** trigger set rather than any config field, so
    /// the check cannot go quiet because of where the emoji was declared.
    #[test]
    fn reaction_triggers_without_their_scope_are_reported() {
        let warnings = scope_warnings(
            &owned(&["chat:write", "im:write", "users:read"]),
            &config_with(false),
            &triggers(&["totsuka-test"]),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("reactions:read"), "{}", warnings[0]);
        // The message has to say what to *do*: the scope alone does not tell
        // an operator that a reinstall reissues both tokens.
        assert!(warnings[0].contains("Reinstall"), "{}", warnings[0]);
    }

    /// Silence is the whole point of the split: a config that asks for nothing
    /// extra must not be nagged about scopes it does not use.
    #[test]
    fn a_token_carrying_what_the_config_uses_is_silent() {
        assert!(
            scope_warnings(
                &owned(&["reactions:read", "channels:read"]),
                &config_with(true),
                &triggers(&["totsuka-test"]),
            )
            .is_empty()
        );
        // …and neither feature configured means neither scope is wanted.
        assert!(
            scope_warnings(
                &owned(&["chat:write"]),
                &config_with(false),
                &ReactionTriggers::default(),
            )
            .is_empty()
        );
    }

    /// Either scope resolves a channel name, so a private-only or public-only
    /// install is legitimate; only having neither breaks the prefix rules.
    #[test]
    fn channel_groups_accept_either_channel_scope() {
        assert!(
            scope_warnings(
                &owned(&["groups:read"]),
                &config_with(true),
                &ReactionTriggers::default(),
            )
            .is_empty(),
            "private-channel-only is a real setup"
        );
        let warnings = scope_warnings(
            &owned(&["chat:write"]),
            &config_with(true),
            &ReactionTriggers::default(),
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("channel_groups"), "{}", warnings[0]);
    }
}
