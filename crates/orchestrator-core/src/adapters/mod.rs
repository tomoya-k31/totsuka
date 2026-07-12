//! Adapters layer: concrete implementations of the [`ports`](crate::ports).
//!
//! JSON-RPC plugin bridge, SQLite persistence, Keychain secret store, etc.
//! Filled in incrementally by feature tasks.

pub mod plugin_host;
pub mod run_lock;
pub mod state_db;

pub use plugin_host::{HostError, Plugin, PluginSpec};
pub use run_lock::{LockError, RunLock};
pub use state_db::{NewTask, StateDb, StateError, TaskRecord};
