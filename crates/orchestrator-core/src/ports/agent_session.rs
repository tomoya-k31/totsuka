//! Agent-session re-attach port (F-37, §5.3).
//!
//! On restart the Orchestrator must reconnect to agent sessions that were
//! in-flight when it exited. This port abstracts "reach the owning plugin and
//! re-attach to a session", so the [`recovery`](crate::recovery) pipeline is
//! testable with a fake and the real transport (the JSON-RPC plugin host) stays
//! in [`adapters::agent_session`](crate::adapters::agent_session).

use std::future::Future;

use plugin_protocol::methods::AgentState;

/// The outcome of a re-attach attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
    /// Re-attached; the agent reports it is in `state` (F-32).
    Attached(AgentState),
    /// The plugin is reachable but the session no longer exists (§5.3): the
    /// agent process that owned it is gone. Not a transport failure.
    Lost,
}

/// Errors from a re-attach attempt. Both variants (like [`AttachOutcome::Lost`])
/// route a task to human confirmation rather than auto-failing it (§5.3).
#[derive(Debug, thiserror::Error)]
pub enum AgentSessionError {
    /// The owning plugin could not be reached (not launched / not enabled).
    #[error("plugin `{plugin}` is unavailable for re-attach: {reason}")]
    Unavailable {
        /// Plugin instance name.
        plugin: String,
        /// Why it is unavailable.
        reason: String,
    },
    /// The re-attach RPC itself failed (transport, timeout, protocol error).
    #[error("re-attach to plugin `{plugin}` failed: {reason}")]
    Attach {
        /// Plugin instance name.
        plugin: String,
        /// The transport/RPC failure.
        reason: String,
    },
}

/// Re-attaches to persisted agent sessions on restart (F-37).
///
/// A successful [`AttachOutcome::Attached`] also implies the state/log stream
/// has been re-established (`state/subscribe`), so the caller can resume
/// consuming it.
pub trait AgentSession: Send + Sync {
    /// Re-attach to `session_id` on `plugin`, reporting the agent's state.
    fn attach(
        &self,
        plugin: &str,
        session_id: &str,
    ) -> impl Future<Output = Result<AttachOutcome, AgentSessionError>> + Send;
}
