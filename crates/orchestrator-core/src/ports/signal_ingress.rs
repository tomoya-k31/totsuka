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
use crate::domain::state::TaskState;

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

/// Which task operation a `POST /task/<op>` control request asks for (#760).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOp {
    /// `POST /task/cancel` — the same transition `totsuka task cancel` makes.
    Cancel,
    /// `POST /task/retry` — the same requeue `totsuka task retry` makes.
    Retry,
}

/// The engine's answer to a `POST /task/cancel` / `POST /task/retry` control
/// request (#760).
///
/// Same convention as [`FocusOutcome`]: a refusal the operator can act on
/// (the task is already finished, unknown, not retryable) is a normal answer
/// with `ok: false` and a `reason`, never an error status. HTTP statuses are
/// left to auth, framing, and an engine that is no longer answering.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskControlOutcome {
    /// Whether the transition was applied.
    pub ok: bool,
    /// The state the task was in before — what callers phrase their notes
    /// from (a pane that stays open, a skipped claim being re-entered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<TaskState>,
    /// The state the task is in now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    /// Retry only: how many messages of the last dispatch were queued again
    /// (#242).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requeued: Option<usize>,
    /// Why nothing was applied, and what to do instead. `None` when `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TaskControlOutcome {
    /// An applied transition.
    pub fn applied(from: TaskState, state: TaskState, requeued: Option<usize>) -> Self {
        Self {
            ok: true,
            from: Some(from),
            state: Some(state),
            requeued,
            reason: None,
        }
    }

    /// A refusal: nothing changed.
    pub fn refused(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            from: None,
            state: None,
            requeued: None,
            reason: Some(reason.into()),
        }
    }
}

/// Accepts control requests from a driving adapter: `POST /focus` (F-94)
/// and `POST /task/cancel` / `POST /task/retry` (#760).
///
/// One port for all of them because they share everything but the verb: the
/// same socket, the same auth, and the same request-response trip through the
/// engine's event channel.
pub trait ControlPort: Send + Sync {
    /// Ask the engine to focus the task's pane (via the task's agent plugin,
    /// `session/focus`, gated on `pane_control`) and wait for the outcome.
    fn focus(&self, task_id: i64)
    -> impl Future<Output = Result<FocusOutcome, SignalError>> + Send;

    /// Ask the engine to cancel or retry the task and wait for the outcome.
    fn task(
        &self,
        op: TaskOp,
        task_id: i64,
    ) -> impl Future<Output = Result<TaskControlOutcome, SignalError>> + Send;
}
