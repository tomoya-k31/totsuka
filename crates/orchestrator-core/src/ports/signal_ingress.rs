//! Signal-ingress port (#131, #136): the boundary a hook-receiving driving
//! adapter submits normalized [`AgentSignal`]s through.
//!
//! The UDS hook server ([`adapters::hook_uds`](crate::adapters::hook_uds)) is a
//! *driving* adapter — external input arriving at the orchestrator. It must not
//! know about the [`Engine`](crate::run::Engine); it only knows this port. The
//! production implementation
//! ([`adapters::EngineSignalSink`](crate::adapters::EngineSignalSink)) forwards
//! each signal onto the engine's event channel, and tests substitute a fake to
//! assert the adapter's HTTP/normalization behaviour in isolation.

use std::future::Future;

use crate::domain::signal::AgentSignal;

/// Acknowledgement that a signal was accepted for processing.
///
/// Deliberately opaque: acceptance means "handed off to the engine", not
/// "processed". The completion/verification pipeline runs asynchronously (E-04),
/// so the adapter replies `200` the moment a signal is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalAck;

/// Errors from submitting a signal.
#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    /// The engine is no longer accepting signals — its receiver was dropped
    /// (the run loop has shut down). The adapter should stop rather than keep
    /// buffering.
    #[error("signal sink is closed: the engine is no longer running")]
    Closed,
}

/// Accepts normalized hook signals from a driving adapter (#136).
///
/// The `submit` future is `Send` so a hook-receiving server can be
/// `tokio::spawn`ed and hand connections off across the runtime.
pub trait SignalPort: Send + Sync {
    /// Submit a normalized signal for the engine to interpret (#138).
    fn submit(
        &self,
        signal: AgentSignal,
    ) -> impl Future<Output = Result<SignalAck, SignalError>> + Send;
}

/// The engine's answer to a `POST /focus` control request (F-94).
///
/// Unlike a signal, focus is request-response: the adapter waits for this
/// outcome and serializes it back to the caller (`totsuka focus`). "Not
/// focused" is a normal answer, not an error — the pane may simply be gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FocusOutcome {
    /// Whether the task's pane ended up focused.
    pub focused: bool,
    /// Why it was not focused (task unknown, no session, capability missing,
    /// pane closed, …). `None` when `focused` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl FocusOutcome {
    /// A successful focus.
    pub fn focused() -> Self {
        Self {
            focused: true,
            reason: None,
        }
    }

    /// A degraded (but normal) "could not focus" answer.
    pub fn not(reason: impl Into<String>) -> Self {
        Self {
            focused: false,
            reason: Some(reason.into()),
        }
    }
}

/// Accepts `POST /focus` control requests from a driving adapter (F-94):
/// bring the pane of `task_id`'s session to the foreground via the task's
/// agent plugin (`session/focus`, gated on `pane_control`).
pub trait FocusPort: Send + Sync {
    /// Ask the engine to focus the task's pane and wait for the outcome.
    fn focus(&self, task_id: i64)
    -> impl Future<Output = Result<FocusOutcome, SignalError>> + Send;
}
