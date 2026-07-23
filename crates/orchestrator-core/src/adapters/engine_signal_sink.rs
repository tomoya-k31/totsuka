//! [`SignalPort`] implementation that forwards signals to the run loop (#136).
//!
//! The hook receiver ([`hook_uds`](crate::adapters::hook_uds)) is engine-
//! agnostic; this thin adapter bridges its [`SignalPort`] dependency to the
//! [`Engine`](crate::run::Engine)'s event channel by wrapping the same
//! `mpsc::UnboundedSender<PluginEvent>` the plugin forwarders use. It holds no
//! state, so duplicate submissions are simply forwarded twice — deduplication
//! is the DB layer's job (D-05), not this adapter's.

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::domain::signal::AgentSignal;
use crate::ports::signal_ingress::{FocusOutcome, FocusPort, SignalAck, SignalError, SignalPort};
use crate::run::PluginEvent;

/// Submits normalized hook signals onto the engine's event channel.
///
/// `Clone` so a per-connection handler can own a copy; every clone targets the
/// same channel.
#[derive(Clone)]
pub struct EngineSignalSink {
    tx: UnboundedSender<PluginEvent>,
}

impl EngineSignalSink {
    /// Wrap the engine's event sender. `pub(crate)`: only the engine, which
    /// owns the channel, constructs this — `PluginEvent` never crosses the
    /// crate boundary.
    pub(crate) fn new(tx: UnboundedSender<PluginEvent>) -> Self {
        Self { tx }
    }
}

impl SignalPort for EngineSignalSink {
    fn submit(
        &self,
        signal: AgentSignal,
    ) -> impl std::future::Future<Output = Result<SignalAck, SignalError>> + Send {
        // Sending on an unbounded channel is synchronous and non-blocking, so
        // the "immediate 200" contract (E-04) is upheld: no awaiting downstream
        // processing. The only failure is a dropped receiver (engine gone).
        let result = self
            .tx
            .send(PluginEvent::HookSignal(signal))
            .map(|()| SignalAck)
            .map_err(|_| SignalError::Closed);
        async move { result }
    }
}

impl FocusPort for EngineSignalSink {
    fn focus(
        &self,
        task_id: i64,
    ) -> impl std::future::Future<Output = Result<FocusOutcome, SignalError>> + Send {
        // Unlike a signal, focus is request-response (F-94): the outcome comes
        // back over a oneshot the run loop answers when it processes the event.
        // A dropped sender (engine gone) or dropped responder (loop exited
        // before answering) both surface as `Closed`.
        let (respond, outcome) = oneshot::channel();
        let sent = self
            .tx
            .send(PluginEvent::Focus { task_id, respond })
            .map_err(|_| SignalError::Closed);
        async move {
            sent?;
            outcome.await.map_err(|_| SignalError::Closed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::signal::{JobId, SignalEvent, SignalSource};
    use tokio::sync::mpsc;

    fn sample_signal() -> AgentSignal {
        AgentSignal {
            source: SignalSource::AgentHook,
            job_id: JobId::new(1, 2),
            tool_session_id: String::new(),
            prompt_id: String::new(),
            event: SignalEvent::Heartbeat,
            payload: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn submit_forwards_a_hook_signal_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = EngineSignalSink::new(tx);

        sink.submit(sample_signal()).await.unwrap();

        match rx.recv().await {
            Some(PluginEvent::HookSignal(signal)) => {
                assert_eq!(signal.job_id, JobId::new(1, 2));
            }
            other => panic!(
                "expected HookSignal, got {other:?}",
                other = other.is_some()
            ),
        }
    }

    #[tokio::test]
    async fn submit_is_stateless_across_duplicates() {
        // Idempotency is the DB layer's responsibility (D-05); the adapter must
        // forward every call, so two identical submits yield two events.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = EngineSignalSink::new(tx);

        sink.submit(sample_signal()).await.unwrap();
        sink.submit(sample_signal()).await.unwrap();

        assert!(matches!(rx.recv().await, Some(PluginEvent::HookSignal(_))));
        assert!(matches!(rx.recv().await, Some(PluginEvent::HookSignal(_))));
    }

    #[tokio::test]
    async fn submit_reports_closed_when_receiver_dropped() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let sink = EngineSignalSink::new(tx);

        assert!(matches!(
            sink.submit(sample_signal()).await,
            Err(SignalError::Closed)
        ));
    }
}
