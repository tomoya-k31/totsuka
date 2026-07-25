//! Adapters layer: concrete implementations of the [`ports`](crate::ports).
//!
//! JSON-RPC plugin bridge, SQLite persistence, Keychain secret store, etc.
//! Filled in incrementally by feature tasks.

pub mod agent_session;
pub mod clock;
pub mod engine_signal_sink;
pub mod git;
pub mod hook_uds;
pub mod llm;
pub mod plugin_host;
pub mod run_lock;
pub mod state_db;

pub use agent_session::PluginAgentSession;
pub use clock::{ManualClock, SystemClock};
pub use engine_signal_sink::EngineSignalSink;
pub use plugin_host::{HostError, Plugin, PluginSpec};
pub use run_lock::{LockError, RunLock};
pub use state_db::{EventRecord, NewTask, SessionRecord, StateDb, StateError, TaskRecord};
