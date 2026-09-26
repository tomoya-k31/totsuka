//! The stdio server (F-51): an [`AgentIdeHandler`] whose wire protocol —
//! including the ACK-then-`state/notification` order of `state/subscribe`
//! (F-38) — is the SDK's `AgentIdeServer` (#759). Mirrors the herdr plugin's
//! server — the same methods and capabilities — but builds an orca CLI
//! adapter instead of a socket transport. Generic over a [`CliFactory`] so
//! the whole surface is driven against a fake orca CLI.

use plugin_protocol::Capabilities;
use plugin_protocol::jsonrpc::{Error, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, DiagnosticsSnapshotParams,
    DiagnosticsSnapshotResult, InitializeParams, InitializeResult, SessionAttachParams,
    SessionAttachResult, SessionFocusParams, SessionFocusResult, SessionListResult,
    SessionReleaseParams, SessionReleaseResult, StateNotification, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams, TaskDispatchResult,
};
use plugin_sdk::{AgentIdeHandler, not_initialized};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::agent::{HANDOFF_WAIT, OrcaAgent};
use crate::cli::OrcaCli;
use crate::config::OrcaConfig;
use crate::error::OrcaError;
use crate::handoff::EnvHandoff;

/// Builds an orca CLI adapter from config. Abstracted so the server is tested
/// against a fake orca.
pub trait CliFactory {
    /// The CLI adapter this factory produces.
    type Cli: OrcaCli;
    /// Build a CLI adapter for `config`.
    fn build(&self, config: &OrcaConfig) -> Self::Cli;
}

/// The orca agent_ide stdio server.
pub struct Server<F: CliFactory> {
    factory: F,
    agent: Option<OrcaAgent<F::Cli>>,
    /// The base the launch env FIFO directory goes under (#744, one
    /// subdirectory per process); `None` when no XDG base or
    /// `HOME` names one, which fails `initialize`.
    handoff_dir: Option<PathBuf>,
    handoff_wait: Duration,
}

impl<F: CliFactory> Server<F> {
    /// A fresh, uninitialized server.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            agent: None,
            handoff_dir: crate::handoff::default_dir(|k| std::env::var(k).ok()),
            handoff_wait: HANDOFF_WAIT,
        }
    }

    /// Put the launch env FIFOs in `dir` and wait `wait` for each to be read,
    /// instead of the runtime dir and the startup budget. For tests: each
    /// wants its FIFOs apart from the others' to inspect them.
    pub fn with_handoff(mut self, dir: PathBuf, wait: Duration) -> Self {
        self.handoff_dir = Some(dir);
        self.handoff_wait = wait;
        self
    }

    /// The agent `initialize` built, or the not-initialized refusal.
    fn agent(&self) -> Result<&OrcaAgent<F::Cli>, Error> {
        self.agent.as_ref().ok_or_else(not_initialized)
    }
}

impl<F> AgentIdeHandler for Server<F>
where
    F: CliFactory + Send,
    F::Cli: Send + Sync,
{
    /// Read by the SDK only for params that do not parse: before
    /// `initialize` they are answered "initialize first", as this server did
    /// when it checked the agent before reading params.
    fn initialized(&self) -> bool {
        self.agent.is_some()
    }

    async fn initialize(&mut self, init: InitializeParams) -> Result<InitializeResult, Error> {
        let removed = crate::config::removed_keys_in(&init.config);
        if !removed.is_empty() {
            return Err(Error::new(
                error_code::CONFIG_INVALID,
                format!("invalid orca plugin config: {}", removed.join(" ")),
            ));
        }
        let config: OrcaConfig = match serde_json::from_value(init.config) {
            Ok(c) => c,
            Err(e) => {
                return Err(Error::new(
                    error_code::CONFIG_INVALID,
                    format!("invalid orca plugin config: {e}"),
                ));
            }
        };
        let wait = self.handoff_wait;
        let handoff = match self
            .handoff_dir
            .clone()
            .map(|dir| EnvHandoff::open(dir, wait))
        {
            Some(Ok(handoff)) => handoff,
            Some(Err(e)) => {
                return Err(Error::new(
                    error_code::INTERNAL_ERROR,
                    format!("cannot prepare the launch env directory: {e}"),
                ));
            }
            None => {
                return Err(Error::new(
                    error_code::INTERNAL_ERROR,
                    "cannot place the launch env directory: none of XDG_RUNTIME_DIR, \
                         XDG_STATE_HOME or HOME is set to an absolute path",
                ));
            }
        };
        let cli = self.factory.build(&config);
        self.agent = Some(OrcaAgent::new(cli, config, handoff));
        Ok(capabilities_result())
    }

    async fn config_validate(
        &mut self,
        params: ConfigValidateParams,
    ) -> Result<ConfigValidateResult, Error> {
        let raw = params.config;
        // Name the removed keys — `config does not parse` is true but useless
        // for the one change the tool_launch rewrite forces.
        let removed = crate::config::removed_keys_in(&raw);
        if !removed.is_empty() {
            return Ok(validate_result(removed));
        }
        let config: OrcaConfig = match serde_json::from_value(raw) {
            Ok(c) => c,
            // Keep serde's detail: it names an unknown or mistyped key, which is
            // what the operator has to fix (#767's conformance check 6).
            Err(e) => {
                return Ok(validate_result(vec![format!("config does not parse: {e}")]));
            }
        };
        // Connectivity check (F-59): does `orca status` run, and is the
        // runtime behind it up? The CLI answers `status` even with the app
        // closed, so a successful call alone proves only that orca is
        // installed.
        let mut errors = Vec::new();
        let cli = self.factory.build(&config);
        match cli.run(vec!["status".into(), "--json".into()]).await {
            Ok(status) => {
                let reachable = status
                    .get("runtime")
                    .and_then(|r| r.get("reachable"))
                    .and_then(Value::as_bool);
                if reachable == Some(false) {
                    errors.push(
                        "the orca runtime is not reachable → start the Orca app (`orca open`)"
                            .into(),
                    );
                }
            }
            Err(e) => errors.push(format!("orca is not reachable → {e}")),
        }
        Ok(validate_result(errors))
    }

    async fn task_dispatch(
        &mut self,
        params: TaskDispatchParams,
    ) -> Result<TaskDispatchResult, Error> {
        self.agent()?.dispatch(params).await.map_err(rpc_error)
    }

    async fn session_attach(
        &mut self,
        params: SessionAttachParams,
    ) -> Result<SessionAttachResult, Error> {
        self.agent()?
            .attach(&params.session_id)
            .await
            .map_err(rpc_error)
    }

    async fn task_cancel(&mut self, params: TaskCancelParams) -> Result<(), Error> {
        self.agent()?
            .cancel(&params.session_id)
            .await
            .map_err(rpc_error)
    }

    async fn state_subscribe(
        &mut self,
        params: StateSubscribeParams,
    ) -> Result<mpsc::UnboundedReceiver<StateNotification>, Error> {
        self.agent()?
            .start_state_stream(&params.session_id)
            .await
            .map_err(rpc_error)
    }

    async fn session_release(
        &mut self,
        params: SessionReleaseParams,
    ) -> Result<SessionReleaseResult, Error> {
        self.agent()?.release(&params).await.map_err(rpc_error)
    }

    async fn session_focus(
        &mut self,
        params: SessionFocusParams,
    ) -> Result<SessionFocusResult, Error> {
        self.agent()?
            .focus(&params.session_id)
            .await
            .map_err(rpc_error)
    }

    async fn session_list(&mut self) -> Result<SessionListResult, Error> {
        self.agent()?.list_sessions().await.map_err(rpc_error)
    }

    async fn diagnostics_snapshot(
        &mut self,
        params: DiagnosticsSnapshotParams,
    ) -> Result<DiagnosticsSnapshotResult, Error> {
        self.agent()?
            .snapshot(&params.session_id)
            .await
            .map_err(rpc_error)
    }
}

/// The capabilities this plugin declares (F-33) — the herdr plugin's set, now
/// that orca is driven through the same contract. Must mirror `plugin.toml`.
///
/// - `pane_control`: an orca terminal is the pane — `terminal switch` /
///   `close` / `list` answer focus, release and list.
/// - `hook_completion`: the agent is launched from the Orchestrator's
///   `tool_launch`, hook settings and env included, so it reports completion
///   through its hooks; the state stream is only an exit deadman.
/// - `diagnostics_snapshot`: `terminal read --screen`.
fn capabilities_result() -> InitializeResult {
    InitializeResult {
        // No workflow options of its own (#554).
        claimed_options: Vec::new(),
        plugin_version: plugin_version(),
        claimed_repos: Vec::new(),
        capabilities: Capabilities {
            pane_control: true,
            state_stream: true,
            hook_completion: true,
            diagnostics_snapshot: true,
            ..Capabilities::default()
        },
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

/// Map an [`OrcaError`] to a JSON-RPC error carrying its actionable message:
/// an internal error, except a session that could not be resumed
/// (`SESSION_UNRESUMABLE`, #242 — the Orchestrator retries without it) and a
/// dispatch without a `tool_launch` (the caller's own malformed request).
fn rpc_error(error: OrcaError) -> Error {
    let code = match error {
        OrcaError::SessionUnresumable(_) => error_code::SESSION_UNRESUMABLE,
        OrcaError::MissingToolLaunch => error_code::INVALID_PARAMS,
        _ => error_code::INTERNAL_ERROR,
    };
    Error::new(code, error.to_string())
}
