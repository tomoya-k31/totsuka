//! totsuka orchestrator core.
//!
//! Hexagonal architecture. The core of the split is three layers:
//!
//! - [`domain`]: pure domain types and the task state machine.
//! - [`ports`]: trait boundaries that adapters implement — agent-session
//!   re-attach, clock, git, repository classification, secrets, signal/control
//!   ingress and process probing. Plugins are **not** behind a port: the run
//!   loop holds them directly and calls them through
//!   [`Plugin::request`](adapters::plugin_host::Plugin::request), whose
//!   method/params/result pairing lives in [`plugin_protocol::rpc`] (#757).
//! - [`adapters`]: concrete implementations (JSON-RPC plugin host, SQLite,
//!   Keychain, ...).

pub mod adapters;
pub mod agent_tools;
pub mod config;
pub mod domain;
pub mod hooks;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod plugins;
pub mod ports;
pub mod prompts;
pub mod recovery;
pub mod repo_select;
pub mod run;
pub mod scheduler;
pub mod task_control;
pub mod template;
pub mod terminal;
pub mod tool;
pub mod worktree;
