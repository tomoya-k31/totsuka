//! # totsuka plugin protocol
//!
//! The stable contract between the totsuka Orchestrator and third-party
//! plugins. This crate is the **single source of truth** for the plugin
//! boundary; plugin authors depend on it to build a compatible plugin.
//!
//! ## Overview
//!
//! Plugins run as **separate processes** and speak **JSON-RPC 2.0 over stdio**,
//! framed as [NDJSON](jsonrpc) (one JSON message per line). There are three
//! plugin [kinds](manifest::PluginKind): `task_source`, `agent_ide`, and
//! `notifier`.
//!
//! A plugin ships a [`plugin.toml`](manifest::Manifest) declaring its name,
//! kind, version, the [protocol range](version) it supports, and its
//! [capabilities](manifest::Capabilities).
//!
//! ## Lifecycle
//!
//! 1. The Orchestrator launches the plugin process and sends
//!    [`initialize`](methods::InitializeParams) with the resolved
//!    plugin config and its [`PROTOCOL_VERSION`];
//!    the plugin replies with its [`capabilities`](methods::InitializeResult).
//! 2. The Orchestrator calls kind-specific methods (see [`method`]).
//! 3. On teardown the Orchestrator calls [`shutdown`](method::SHUTDOWN).
//!
//! ## Methods by kind (§11)
//!
//! | Kind | Methods |
//! |---|---|
//! | common | [`initialize`](method::INITIALIZE), [`shutdown`](method::SHUTDOWN), [`config/validate`](method::CONFIG_VALIDATE) |
//! | `task_source` | [`tasks/fetch`](method::TASKS_FETCH), [`task/update_status`](method::TASK_UPDATE_STATUS), [`result/publish`](method::RESULT_PUBLISH) |
//! | `agent_ide` | [`task/dispatch`](method::TASK_DISPATCH), [`task/cancel`](method::TASK_CANCEL), [`session/attach`](method::SESSION_ATTACH), [`state/subscribe`](method::STATE_SUBSCRIBE) → [`state/notification`](method::STATE_NOTIFICATION) |
//! | `notifier` | [`notify`](method::NOTIFY) |
//!
//! ## Responsibility boundary (F-86)
//!
//! An `agent_ide` plugin's responsibility ends **at the commit**. Pushing the
//! branch and opening the pull request is the **Orchestrator's** job — plugins
//! must never push or open PRs.
//!
//! ## Versioning (§10.2)
//!
//! The protocol has its own [`PROTOCOL_VERSION`],
//! independent of the totsuka application version. A plugin declares the range
//! it supports; the Orchestrator refuses incompatible plugins (F-54).
//!
//! ## Example: encode a request and decode a response
//!
//! ```
//! use plugin_protocol::{Request, Response, method};
//! use plugin_protocol::methods::{InitializeParams, InitializeResult};
//! use plugin_protocol::version::protocol_version;
//! use plugin_protocol::jsonrpc::to_line;
//!
//! // Orchestrator side: build an `initialize` request as one NDJSON line.
//! let params = InitializeParams {
//!     protocol_version: protocol_version(),
//!     config: serde_json::json!({ "socket_path": "/run/herdr.sock" }),
//!     repositories: vec![],
//! };
//! let request = Request::new(1, method::INITIALIZE, Some(serde_json::to_value(&params)?));
//! let line = to_line(&request)?; // send `line + "\n"` to the plugin's stdin
//! assert!(line.starts_with(r#"{"jsonrpc":"2.0""#));
//!
//! // Plugin side: parse the request and reply with capabilities.
//! let parsed: Request = serde_json::from_str(&line)?;
//! assert_eq!(parsed.method, "initialize");
//! let result = InitializeResult {
//!     plugin_version: semver::Version::new(1, 2, 0),
//!     capabilities: Default::default(),
//! };
//! let response = Response::result(parsed.id, serde_json::to_value(&result)?);
//! assert!(!response.is_error());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod jsonrpc;
pub mod manifest;
pub mod methods;
pub mod task;
pub mod version;

pub use jsonrpc::{Error, Notification, Request, RequestId, Response, error_code};
pub use manifest::{Capabilities, Manifest, ManifestError, OutputCapability, PluginKind};
pub use methods::method;
pub use methods::{
    AgentState, ConfigValidateParams, ConfigValidateResult, ExecutionMode, InitializeParams,
    InitializeResult, NotifierEvent, NotifyParams, RepoInfo, ResultPublishParams,
    SessionAttachParams, SessionAttachResult, StateNotification, StateSubscribeParams,
    TaskCancelParams, TaskDispatchParams, TaskDispatchResult, TaskUpdateStatusParams,
    TasksFetchParams, TasksFetchResult,
};
pub use task::Task;
pub use version::{PROTOCOL_VERSION, is_compatible, is_compatible_with_current, protocol_version};
