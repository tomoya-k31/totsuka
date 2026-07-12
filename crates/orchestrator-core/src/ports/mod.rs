//! Ports layer: trait boundaries that adapters implement.
//!
//! The swap-prone boundaries (`TaskSource`, `AgentIde`, `LlmRouter`,
//! `SecretStore`, persistence) live here so their implementations can be
//! replaced without touching the domain. Filled in incrementally by feature
//! tasks.

pub mod process;
pub mod secret;

pub use process::ProcessProbe;
pub use secret::{SecretError, SecretRef, SecretStore, SecretString};
