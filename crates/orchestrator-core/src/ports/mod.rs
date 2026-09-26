//! Ports layer: trait boundaries that adapters implement.
//!
//! The swap-prone boundaries live here so their implementations can be
//! replaced — or faked in tests — without touching the domain. There are
//! eight: [`AgentSession`], [`Clock`], [`GitRunner`], [`RepoClassifier`],
//! [`SecretStore`], [`SignalPort`], [`ControlPort`] and [`ProcessProbe`].
//!
//! Plugins (`task_source` / `agent_ide` / `notifier`) and persistence have no
//! port. The run loop uses the plugin host and the state DB directly; what
//! keeps plugin calls honest is the typed descriptor table in
//! [`plugin_protocol::rpc`], not a trait here (#757, ADR-0101).

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
pub use llm::{ClassifyError, RepoClassifier};
pub use process::ProcessProbe;
pub use secret::{SecretError, SecretRef, SecretStore, SecretString, is_secret_reference};
pub use signal_ingress::{
    ControlPort, FocusOutcome, SignalAck, SignalError, SignalPort, TaskControlOutcome, TaskOp,
};
