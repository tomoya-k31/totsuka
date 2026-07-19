//! Plugin host: launch a plugin as a subprocess and speak JSON-RPC 2.0 over its
//! stdio (F-51), with request correlation, timeouts, and crash isolation (§5.3).
//!
//! # Lifecycle
//!
//! 1. [`Plugin::launch`] checks the manifest's protocol compatibility (F-54),
//!    spawns the binary, wires stdin/stdout to an NDJSON transport, forwards
//!    stderr to the log, then sends [`initialize`](plugin_protocol::method::INITIALIZE)
//!    (with the resolved config + secrets, F-65) and records the plugin's
//!    declared capabilities (F-33).
//! 2. [`Plugin::call`] issues requests, correlating responses by id with a
//!    per-call timeout.
//! 3. [`Plugin::shutdown`] sends `shutdown`, waits a grace period, then kills.
//!
//! # Crash isolation (§5.3)
//!
//! If the child exits, its stdout closes; the reader drains all pending calls
//! with [`HostError::Crashed`] and marks the plugin closed. The host itself is
//! unaffected — the caller decides how to fail the affected tasks (#63). v1
//! does not auto-restart.
//!
//! # Plugin-initiated requests (0.1.6)
//!
//! A plugin may itself issue a request over the same stdio (P→O, e.g.
//! `task/submit`). The reader routes any line carrying both `method` and `id`
//! to the [`IncomingRequest`] channel ([`Plugin::take_incoming_requests`]);
//! the consumer answers through the carried [`Responder`], which serializes
//! the reply onto the shared writer. Lines with `method` and no `id` remain
//! notifications; lines with `id` and `result`/`error` remain responses to
//! our own calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use plugin_protocol::jsonrpc::{self, Notification, Request};
use plugin_protocol::manifest::Manifest;
use plugin_protocol::methods::{InitializeParams, InitializeResult};
use plugin_protocol::{Capabilities, version};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

/// How a plugin should be launched.
#[derive(Debug, Clone)]
pub struct PluginSpec {
    /// Plugin instance name (for logs and errors).
    pub name: String,
    /// Path to the plugin binary.
    pub program: PathBuf,
    /// Arguments to pass to the binary.
    pub args: Vec<String>,
    /// The plugin's manifest (used for the F-54 compatibility check).
    pub manifest: Manifest,
    /// Resolved plugin-specific config passed to `initialize` (F-64/F-65).
    pub init_config: Value,
    /// Orchestrator-configured repositories supplied at `initialize`
    /// (#109). Populated for task_source plugins only; empty otherwise
    /// (the field is omitted from the wire when empty).
    pub repositories: Vec<plugin_protocol::methods::RepoInfo>,
    /// The orchestrator's `[llm]` settings supplied at `initialize` as a
    /// source-side classification default (#119). Populated for task_source
    /// plugins only; `None` otherwise (omitted from the wire when unset).
    pub llm: Option<plugin_protocol::methods::LlmInfo>,
    /// Per-call RPC timeout.
    pub timeout: Duration,
}

/// Errors from the plugin host.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The plugin's protocol range excludes the Orchestrator's version (F-54).
    #[error(
        "plugin `{name}` is protocol-incompatible: it supports `{req}` but the orchestrator is {have} → update the plugin or orchestrator"
    )]
    ProtocolMismatch {
        /// Plugin name.
        name: String,
        /// The plugin's declared requirement.
        req: String,
        /// The Orchestrator's protocol version.
        have: String,
    },
    /// Spawning the process failed.
    #[error("failed to spawn plugin `{name}`: {source} → check the binary path and permissions")]
    Spawn {
        /// Plugin name.
        name: String,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// The plugin process exited/crashed.
    #[error("plugin `{0}` crashed or exited")]
    Crashed(String),
    /// The plugin is closed and cannot accept calls.
    #[error("plugin `{0}` is closed")]
    Closed(String),
    /// A call exceeded its timeout.
    #[error("plugin `{name}` call `{method}` timed out after {secs}s")]
    Timeout {
        /// Plugin name.
        name: String,
        /// Method that timed out.
        method: String,
        /// Timeout in seconds.
        secs: u64,
    },
    /// The plugin returned a JSON-RPC error.
    #[error("plugin `{name}` method `{method}` failed ({code}): {message}")]
    Rpc {
        /// Plugin name.
        name: String,
        /// Method name.
        method: String,
        /// JSON-RPC error code.
        code: i64,
        /// Error message.
        message: String,
    },
    /// (De)serialization failed.
    #[error("json error talking to plugin: {0}")]
    Json(#[from] serde_json::Error),
    /// IO error on the transport.
    #[error("io error talking to plugin: {0}")]
    Io(#[from] std::io::Error),
}

/// Outcome delivered to a waiting call: RPC success value or RPC error.
type PendingOutcome = Result<Value, jsonrpc::Error>;

/// A request initiated by the plugin (P→O, 0.1.6 — e.g. `task/submit`),
/// surfaced to the consumer of [`Plugin::take_incoming_requests`] together
/// with the [`Responder`] that answers it.
#[derive(Debug)]
pub struct IncomingRequest {
    /// Method name (e.g. `task/submit`).
    pub method: String,
    /// Raw params, if any (the consumer parses the typed shape).
    pub params: Option<Value>,
    /// Answer channel; consume with [`Responder::ok`] / [`Responder::err`].
    pub responder: Responder,
}

/// Writes the JSON-RPC response for one plugin-initiated request back onto
/// the plugin's shared writer (so replies never interleave with concurrent
/// O→P request lines).
///
/// Dropping a `Responder` without answering sends nothing — the plugin's own
/// call timeout covers that case. After a crash the writer is gone and both
/// sends become harmless no-ops.
#[derive(Debug)]
pub struct Responder {
    id: jsonrpc::RequestId,
    write_tx: mpsc::UnboundedSender<String>,
}

impl Responder {
    /// Answer the request with a success result.
    pub fn ok(self, result: Value) {
        self.send(jsonrpc::Response::result(self.id.clone(), result));
    }

    /// Answer the request with a JSON-RPC error.
    pub fn err(self, error: jsonrpc::Error) {
        self.send(jsonrpc::Response::error(self.id.clone(), error));
    }

    fn send(&self, response: jsonrpc::Response) {
        match jsonrpc::to_line(&response) {
            Ok(line) => {
                let _ = self.write_tx.send(line);
            }
            Err(e) => tracing::warn!("failed to encode response to plugin request: {e}"),
        }
    }
}

/// Shared, cloneable inner state used by the transport tasks and callers.
struct Inner {
    name: String,
    write_tx: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<i64, oneshot::Sender<PendingOutcome>>>,
    next_id: AtomicI64,
    closed: AtomicBool,
    timeout: Duration,
}

impl Inner {
    async fn call_raw(&self, method: &str, params: Option<Value>) -> Result<Value, HostError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostError::Closed(self.name.clone()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            // The closed-check and the insert must be atomic against the
            // reader's crash drain (it stores `closed` *then* clears `pending`).
            // Without this, a call that starts during the crash window would
            // insert into a map that is never drained again and block for the
            // full timeout, misreporting a crash as a timeout.
            let mut pending = self.pending.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err(HostError::Crashed(self.name.clone()));
            }
            pending.insert(id, tx);
        }

        let request = Request::new(id, method, params);
        let line = jsonrpc::to_line(&request)?;
        if self.write_tx.send(line).is_err() {
            // The writer task is gone (stdin dead) => the plugin is unusable.
            // Mark it closed so subsequent calls short-circuit.
            self.closed.store(true, Ordering::Release);
            self.pending.lock().await.remove(&id);
            return Err(HostError::Closed(self.name.clone()));
        }

        match tokio::time::timeout(self.timeout, rx).await {
            // Timed out: forget the pending entry.
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(HostError::Timeout {
                    name: self.name.clone(),
                    method: method.to_string(),
                    secs: self.timeout.as_secs(),
                })
            }
            // Sender dropped without a value: the plugin crashed.
            Ok(Err(_)) => Err(HostError::Crashed(self.name.clone())),
            // RPC error response.
            Ok(Ok(Err(e))) => Err(HostError::Rpc {
                name: self.name.clone(),
                method: method.to_string(),
                code: e.code,
                message: e.message,
            }),
            // Success.
            Ok(Ok(Ok(value))) => Ok(value),
        }
    }
}

/// A launched, initialized plugin.
pub struct Plugin {
    inner: Arc<Inner>,
    child: Mutex<Child>,
    /// Notifications streamed by the plugin (`state/subscribe`, F-38).
    notifications: Mutex<Option<mpsc::UnboundedReceiver<Notification>>>,
    /// Requests initiated by the plugin (P→O, 0.1.6 — `task/submit`).
    incoming: Mutex<Option<mpsc::UnboundedReceiver<IncomingRequest>>>,
    capabilities: Capabilities,
    plugin_version: semver::Version,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("name", &self.inner.name)
            .field("plugin_version", &self.plugin_version)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// Launch and initialize a plugin.
    pub async fn launch(spec: PluginSpec) -> Result<Self, HostError> {
        // 1. Protocol compatibility (F-54) — fail fast before spawning.
        let orchestrator = version::protocol_version();
        if !spec.manifest.is_compatible_with(&orchestrator) {
            return Err(HostError::ProtocolMismatch {
                name: spec.name.clone(),
                req: spec.manifest.protocol_version.to_string(),
                have: orchestrator.to_string(),
            });
        }

        // 2. Spawn the process with piped stdio.
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| HostError::Spawn {
                name: spec.name.clone(),
                source,
            })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let (write_tx, write_rx) = mpsc::unbounded_channel::<String>();
        let (notif_tx, notif_rx) = mpsc::unbounded_channel::<Notification>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<IncomingRequest>();
        let inner = Arc::new(Inner {
            name: spec.name.clone(),
            write_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            timeout: spec.timeout,
        });

        spawn_writer(stdin, write_rx);
        spawn_reader(
            spec.name.clone(),
            stdout,
            inner.clone(),
            notif_tx,
            incoming_tx,
        );
        spawn_stderr_logger(spec.name.clone(), stderr);

        let plugin = Self {
            inner,
            child: Mutex::new(child),
            notifications: Mutex::new(Some(notif_rx)),
            incoming: Mutex::new(Some(incoming_rx)),
            capabilities: Capabilities::default(),
            plugin_version: semver::Version::new(0, 0, 0),
        };

        // 3. initialize (F-65: config already has secrets resolved).
        let init = InitializeParams {
            protocol_version: orchestrator,
            config: spec.init_config,
            repositories: spec.repositories,
            llm: spec.llm,
            // 0.1.6: workflow triggers + cadence for push sources; wired from
            // the orchestrator config in a follow-up (#185), empty until then.
            triggers: vec![],
            poll_interval_secs: None,
        };
        let result: InitializeResult = plugin
            .call(plugin_protocol::method::INITIALIZE, &init)
            .await?;

        Ok(Self {
            capabilities: result.capabilities,
            plugin_version: result.plugin_version,
            ..plugin
        })
    }

    /// The plugin's declared capabilities (F-33).
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// The plugin's own version (from `initialize`).
    pub fn plugin_version(&self) -> &semver::Version {
        &self.plugin_version
    }

    /// The plugin instance name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Whether the plugin process has exited.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Call a method with typed params, deserializing the typed result.
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, HostError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let value = self
            .inner
            .call_raw(method, Some(serde_json::to_value(params)?))
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Call a method with no params, deserializing the typed result.
    pub async fn call_no_params<R>(&self, method: &str) -> Result<R, HostError>
    where
        R: DeserializeOwned,
    {
        let value = self.inner.call_raw(method, None).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Send a fire-and-forget notification (e.g. `notify`, F-90).
    pub fn notify<P: Serialize>(&self, method: &str, params: &P) -> Result<(), HostError> {
        let note = Notification::new(method, Some(serde_json::to_value(params)?));
        let line = jsonrpc::to_line(&note)?;
        self.inner
            .write_tx
            .send(line)
            .map_err(|_| HostError::Closed(self.inner.name.clone()))
    }

    /// Take the notification receiver (once) to consume the plugin's stream.
    pub async fn take_notifications(&self) -> Option<mpsc::UnboundedReceiver<Notification>> {
        self.notifications.lock().await.take()
    }

    /// Take the incoming-request receiver (once) to consume the plugin's
    /// P→O requests (0.1.6, `task/submit`). The channel closes when the
    /// plugin exits (the reader task drops the sender).
    pub async fn take_incoming_requests(&self) -> Option<mpsc::UnboundedReceiver<IncomingRequest>> {
        self.incoming.lock().await.take()
    }

    /// Ask the plugin to validate a plugin-specific config (F-59).
    pub async fn config_validate(
        &self,
        config: Value,
    ) -> Result<plugin_protocol::methods::ConfigValidateResult, HostError> {
        let params = plugin_protocol::methods::ConfigValidateParams { config };
        self.call(plugin_protocol::method::CONFIG_VALIDATE, &params)
            .await
    }

    /// Gracefully shut down: send `shutdown`, wait `grace`, then kill.
    pub async fn shutdown(&self, grace: Duration) -> Result<(), HostError> {
        // Best-effort shutdown notification; ignore errors if already closed.
        let shutdown = Request::new(0, plugin_protocol::method::SHUTDOWN, None);
        if let Ok(line) = jsonrpc::to_line(&shutdown) {
            let _ = self.inner.write_tx.send(line);
        }
        let mut child = self.child.lock().await;
        let outcome = match tokio::time::timeout(grace, child.wait()).await {
            // Exited within grace: propagate any wait() IO error.
            Ok(result) => result.map(|_status| ()),
            // Timed out: force kill, then reap (propagating that wait's error).
            Err(_elapsed) => {
                let _ = child.start_kill();
                child.wait().await.map(|_status| ())
            }
        };
        self.inner.closed.store(true, Ordering::Release);
        outcome.map_err(HostError::Io)
    }
}

/// Names of plugins that may be launched: **enabled entries only** (F-58).
///
/// The host is a pure mechanism — it launches whatever [`PluginSpec`] it is
/// given — so the guarantee that a disabled plugin is never started is enforced
/// here, at the decision point callers (`run`/`config validate`) use to build
/// their spec list.
pub fn launchable_plugin_names(config: &crate::config::RootConfig) -> Vec<String> {
    config
        .plugins
        .iter()
        .filter(|(_, p)| p.enabled)
        .map(|(name, _)| name.clone())
        .collect()
}

/// Grace period for a config-validate probe's shutdown.
const VALIDATE_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Validate config by temporarily launching each plugin and delegating to its
/// `config/validate` (F-59). Each plugin is shut down afterwards. This is the
/// online part of `config validate`, skipped under `--offline` (F-63).
///
/// Returns one entry per spec: the plugin name and either its validation
/// result or the host error (spawn/protocol/timeout/crash).
pub async fn validate_all(
    specs: Vec<(PluginSpec, Value)>,
) -> Vec<(
    String,
    Result<plugin_protocol::methods::ConfigValidateResult, HostError>,
)> {
    let mut out = Vec::with_capacity(specs.len());
    for (spec, config) in specs {
        let name = spec.name.clone();
        let result = async {
            let plugin = Plugin::launch(spec).await?;
            let validation = plugin.config_validate(config).await;
            let _ = plugin.shutdown(VALIDATE_SHUTDOWN_GRACE).await;
            validation
        }
        .await;
        out.push((name, result));
    }
    out
}

/// Writer task: drain the outgoing channel to the plugin's stdin as NDJSON.
fn spawn_writer(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
                || stdin.flush().await.is_err()
            {
                break;
            }
        }
    });
}

/// Reader task: parse NDJSON from stdout, routing responses to waiting calls,
/// plugin-initiated requests (`method` + `id`, 0.1.6) to the incoming-request
/// channel, and notifications to the notification channel. On EOF (child exit)
/// it drains all pending calls so they resolve as [`HostError::Crashed`]
/// (§5.3) and drops both channel senders so consumers observe the close.
fn spawn_reader(
    name: String,
    stdout: tokio::process::ChildStdout,
    inner: Arc<Inner>,
    notif_tx: mpsc::UnboundedSender<Notification>,
    incoming_tx: mpsc::UnboundedSender<IncomingRequest>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                tracing::warn!(plugin = %name, "ignoring non-JSON line from plugin");
                continue;
            };
            let is_response = value.get("id").is_some()
                && (value.get("result").is_some() || value.get("error").is_some());
            if is_response {
                deliver_response(&inner, value).await;
            } else if value.get("method").is_some() && value.get("id").is_some() {
                // A plugin-initiated request (P→O, 0.1.6). This branch must
                // come before the notification one: a `Notification` parse
                // would silently accept the value and drop its `id`.
                match serde_json::from_value::<Request>(value) {
                    Ok(request) => {
                        let _ = incoming_tx.send(IncomingRequest {
                            method: request.method,
                            params: request.params,
                            responder: Responder {
                                id: request.id,
                                write_tx: inner.write_tx.clone(),
                            },
                        });
                    }
                    Err(e) => {
                        tracing::warn!(plugin = %name, "ignoring malformed request from plugin: {e}");
                    }
                }
            } else if value.get("method").is_some()
                && let Ok(note) = serde_json::from_value::<Notification>(value)
            {
                let _ = notif_tx.send(note);
            }
        }
        // stdout closed -> the plugin exited. Mark closed and fail all waiters.
        inner.closed.store(true, Ordering::Release);
        let mut pending = inner.pending.lock().await;
        pending.clear(); // dropping senders => callers observe Crashed
        tracing::warn!(plugin = %name, "plugin process closed its output; marked closed");
    });
}

/// Deliver a parsed response object to its waiting call, if any.
async fn deliver_response(inner: &Arc<Inner>, value: Value) {
    // The id is a number for our requests; ignore responses we can't correlate.
    let Some(id) = value.get("id").and_then(Value::as_i64) else {
        return;
    };
    let outcome: PendingOutcome = if let Some(err) = value.get("error") {
        match serde_json::from_value::<jsonrpc::Error>(err.clone()) {
            Ok(e) => Err(e),
            Err(_) => Err(jsonrpc::Error::new(
                jsonrpc::error_code::INTERNAL_ERROR,
                "malformed error object",
            )),
        }
    } else {
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    };
    if let Some(tx) = inner.pending.lock().await.remove(&id) {
        let _ = tx.send(outcome);
    }
}

/// Forward plugin stderr lines into the Orchestrator log (F-38 adjacent).
fn spawn_stderr_logger(name: String, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(plugin = %name, "{line}");
        }
    });
}
