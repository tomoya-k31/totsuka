//! totsuka orchestrator core.
//!
//! Hexagonal architecture skeleton. The crate is split into three layers:
//!
//! - [`domain`]: pure domain types and the task state machine.
//! - [`ports`]: trait boundaries (`TaskSource`, `AgentIde`, `LlmRouter`,
//!   `SecretStore`, ...) that adapters implement.
//! - [`adapters`]: concrete implementations (JSON-RPC plugin bridge, SQLite,
//!   Keychain, ...).
//!
//! Individual features are filled in by later tasks; this task only lays down
//! the module skeleton.

pub mod adapters;
pub mod config;
pub mod domain;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod plugins;
pub mod ports;
pub mod recovery;
pub mod repo_select;
pub mod run;
pub mod scheduler;
pub mod worktree;
