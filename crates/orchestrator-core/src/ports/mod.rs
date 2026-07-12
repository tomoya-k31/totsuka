//! Ports layer: trait boundaries that adapters implement.
//!
//! The swap-prone boundaries (`TaskSource`, `AgentIde`, `LlmRouter`,
//! `SecretStore`, persistence) live here so their implementations can be
//! replaced without touching the domain. Filled in by later tasks.
