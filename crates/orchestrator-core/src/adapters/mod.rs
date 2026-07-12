//! Adapters layer: concrete implementations of the [`ports`](crate::ports).
//!
//! JSON-RPC plugin bridge, SQLite persistence, Keychain secret store, etc.
//! Filled in incrementally by feature tasks.

pub mod agent_session;
pub mod git;
pub mod llm;
pub mod plugin_host;
pub mod run_lock;
pub mod state_db;

pub use agent_session::PluginAgentSession;
pub use plugin_host::{HostError, Plugin, PluginSpec};
pub use run_lock::{LockError, RunLock};
pub use state_db::{NewTask, SessionRecord, StateDb, StateError, TaskRecord};
