//! Ports layer: trait boundaries that adapters implement.
//!
//! The swap-prone boundaries (`TaskSource`, `AgentIde`, `LlmRouter`,
//! `SecretStore`, persistence) live here so their implementations can be
//! replaced without touching the domain. Filled in incrementally by feature
//! tasks.

pub mod agent_session;
pub mod clock;
pub mod git;
pub mod llm;
pub mod process;
pub mod secret;
pub mod signal_ingress;

pub use agent_session::{AgentSession, AgentSessionError, AttachOutcome};
pub use clock::Clock;
pub use git::{GitOutput, GitRunner};
pub use llm::{ChatRequest, LlmError, LlmRouter};
pub use process::ProcessProbe;
pub use secret::{SecretError, SecretRef, SecretStore, SecretString};
pub use signal_ingress::{FocusOutcome, FocusPort, SignalAck, SignalError, SignalPort};
