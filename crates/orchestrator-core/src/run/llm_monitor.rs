//! The LLM gateway as the run loop sees it (F-110 / F-111): the classifier,
//! the health record it writes, and the one liveness probe that may be in
//! flight (#758).
//!
//! One type so "an LLM is configured" is a single `Option`: a classifier
//! without its health record, or a probe without a classifier, cannot be
//! represented. The health record is the one [`MonitoredClassifier`] already
//! owns — there is no second handle to keep in step.
//!
//! Per ADR-0103 this calls no plugin, DB or git. It does spawn the probe:
//! the classifier sits behind the [`RepoClassifier`] port, and owning the
//! handle here is what keeps "at most one probe" and "abort it on resume" in
//! one place.

use std::sync::Arc;
use std::time::Duration;

use crate::adapters::llm::{LlmHealth, MonitoredClassifier};
use crate::ports::llm::RepoClassifier;

pub(super) struct LlmMonitor<L> {
    /// The repository classifier, wrapped so every call and probe updates
    /// [`LlmHealth`]. Shared, because the probe runs on a spawned task.
    classifier: Arc<MonitoredClassifier<L>>,
    /// The liveness probe in flight, if any — at most one at a time, so an
    /// unanswering gateway is asked once per interval, not once per tick.
    probe: Option<tokio::task::JoinHandle<()>>,
}

impl<L: RepoClassifier + 'static> LlmMonitor<L> {
    pub(super) fn new(classifier: L) -> Self {
        Self {
            classifier: Arc::new(MonitoredClassifier::new(classifier)),
            probe: None,
        }
    }

    /// The classifier repository selection asks.
    pub(super) fn classifier(&self) -> &MonitoredClassifier<L> {
        &self.classifier
    }

    /// The health record the classifier writes, for whoever publishes it.
    pub(super) fn health(&self) -> Arc<LlmHealth> {
        self.classifier.health()
    }

    /// Treat everything believed about the gateway as stale after a resume
    /// from sleep: abort the probe in flight, drop the pooled connections and
    /// forget the last contact, so the next [`probe_if_due`](Self::probe_if_due)
    /// asks at once.
    pub(super) fn on_resume(&mut self) {
        // A probe that left before the nap is stuck on the old pool until its
        // timeout, and while it is unfinished no new one is spawned — so the
        // "immediate" post-resume probe would wait on it. Abort it first.
        if let Some(probe) = self.probe.take() {
            probe.abort();
        }
        self.classifier.reset_connections();
        self.classifier.health().forget_contact();
    }

    /// Spend a liveness probe on the gateway if nothing has heard from it
    /// lately.
    ///
    /// "Lately" is `interval` since the last contact of any kind — real
    /// traffic is the best probe there is and costs nothing extra — shrinking
    /// to `interval_while_unreachable` once the gateway is latched down, so
    /// recovery is noticed within a minute. No contact at all (startup, or a
    /// resume that forgot it) is due at once. At most one probe is in flight;
    /// the classifier wrapper records the outcome, so nothing here awaits it.
    pub(super) fn probe_if_due(
        &mut self,
        interval: Duration,
        interval_while_unreachable: Duration,
    ) {
        if self.probe.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let health = self.classifier.health();
        let interval = if health.unreachable().is_some() {
            interval_while_unreachable
        } else {
            interval
        };
        let due = health
            .last_contact()
            .is_none_or(|last| last.elapsed() >= interval);
        if !due {
            return;
        }
        let classifier = Arc::clone(&self.classifier);
        self.probe = Some(tokio::spawn(async move {
            // The outcome is recorded by the classifier wrapper; the `Result`
            // itself has already been logged there on every state change.
            let _ = classifier.probe().await;
        }));
    }

    /// The probe in flight, for tests that wait on it or inspect it.
    #[cfg(test)]
    pub(super) fn probe_mut(&mut self) -> &mut Option<tokio::task::JoinHandle<()>> {
        &mut self.probe
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::ports::llm::{Classification, ClassifyError, ClassifyRequest};

    const LONG: Duration = Duration::from_secs(3600);

    /// Counts probes and resets; a probe parks until released while `hang`
    /// is set.
    #[derive(Clone, Default)]
    struct Gateway {
        probes: Arc<AtomicUsize>,
        resets: Arc<AtomicUsize>,
        hang: Arc<AtomicBool>,
        gate: Arc<tokio::sync::Notify>,
        down: Arc<AtomicBool>,
    }

    impl RepoClassifier for Gateway {
        async fn classify(&self, _: &ClassifyRequest) -> Result<Classification, ClassifyError> {
            Err(ClassifyError::InvalidResponse("not under test".into()))
        }

        async fn probe(&self) -> Result<(), ClassifyError> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if self.hang.load(Ordering::SeqCst) {
                self.gate.notified().await;
            }
            if self.down.load(Ordering::SeqCst) {
                Err(ClassifyError::Timeout(30))
            } else {
                Ok(())
            }
        }

        fn reset_connections(&self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    async fn settle(monitor: &mut LlmMonitor<Gateway>) {
        if let Some(probe) = monitor.probe_mut().take() {
            probe.await.unwrap();
        }
    }

    #[tokio::test]
    async fn at_most_one_probe_is_in_flight() {
        let gateway = Gateway::default();
        gateway.hang.store(true, Ordering::SeqCst);
        let mut monitor = LlmMonitor::new(gateway.clone());

        monitor.probe_if_due(Duration::ZERO, Duration::ZERO);
        tokio::task::yield_now().await;
        monitor.probe_if_due(Duration::ZERO, Duration::ZERO);
        tokio::task::yield_now().await;
        assert_eq!(gateway.probes.load(Ordering::SeqCst), 1, "still in flight");

        gateway.hang.store(false, Ordering::SeqCst);
        gateway.gate.notify_one();
        settle(&mut monitor).await;
        monitor.probe_if_due(Duration::ZERO, Duration::ZERO);
        settle(&mut monitor).await;
        assert_eq!(
            gateway.probes.load(Ordering::SeqCst),
            2,
            "finished → may ask again"
        );
    }

    #[tokio::test]
    async fn fresh_contact_postpones_the_probe_until_the_interval() {
        let gateway = Gateway::default();
        let mut monitor = LlmMonitor::new(gateway.clone());
        monitor.probe_if_due(LONG, LONG);
        settle(&mut monitor).await;
        assert_eq!(
            gateway.probes.load(Ordering::SeqCst),
            1,
            "no contact yet → due"
        );

        monitor.probe_if_due(LONG, LONG);
        assert!(monitor.probe_mut().is_none(), "the answer is fresh");
    }

    #[tokio::test]
    async fn a_latched_down_gateway_uses_the_shorter_interval() {
        let gateway = Gateway::default();
        gateway.down.store(true, Ordering::SeqCst);
        let mut monitor = LlmMonitor::new(gateway.clone());
        monitor.probe_if_due(LONG, Duration::ZERO);
        settle(&mut monitor).await;
        assert!(monitor.health().unreachable().is_some());

        monitor.probe_if_due(LONG, Duration::ZERO);
        settle(&mut monitor).await;
        assert_eq!(
            gateway.probes.load(Ordering::SeqCst),
            2,
            "down → re-asked at once"
        );
    }

    #[tokio::test]
    async fn a_resume_aborts_the_probe_resets_and_forgets_contact() {
        let gateway = Gateway::default();
        let mut monitor = LlmMonitor::new(gateway.clone());
        monitor.probe_if_due(LONG, LONG);
        settle(&mut monitor).await;
        assert!(monitor.health().last_contact().is_some());

        gateway.hang.store(true, Ordering::SeqCst);
        monitor.health().forget_contact();
        monitor.probe_if_due(LONG, LONG);
        tokio::task::yield_now().await;
        let stuck = monitor.probe_mut().as_ref().unwrap().abort_handle();

        monitor.health().record(&Ok(()));
        monitor.on_resume();
        tokio::task::yield_now().await;
        assert!(stuck.is_finished(), "the stuck probe was aborted");
        assert_eq!(gateway.resets.load(Ordering::SeqCst), 1);
        assert!(
            monitor.health().last_contact().is_none(),
            "contact forgotten"
        );

        gateway.hang.store(false, Ordering::SeqCst);
        monitor.probe_if_due(LONG, LONG);
        settle(&mut monitor).await;
        assert_eq!(
            gateway.probes.load(Ordering::SeqCst),
            3,
            "asked again at once"
        );
    }
}
