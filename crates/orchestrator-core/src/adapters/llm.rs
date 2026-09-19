//! The Orchestrator's side of repository classification (F-12, F-13, F-110,
//! F-111).
//!
//! The classifiers themselves live in the shared `repo-classifier` crate;
//! this module builds the one `[llm]` asks for ([`gateway_classifier`]) and
//! wraps it in [`MonitoredClassifier`], which keeps [`LlmHealth`] current.

use std::time::Duration;

use repo_classifier::{
    ApiKey, ChatClassifier, ChatSettings, Classification, ClassifyError, ClassifyRequest,
    ConfiguredClassifier, DecisionsClassifier, DecisionsSettings, RepoClassifier, ReqwestTransport,
    scrub_urls,
};

use crate::config::{LlmApi, LlmConfig};
use crate::ports::SecretString;

/// The classifier `[llm]` configures.
pub type GatewayClassifier = ConfiguredClassifier<ReqwestTransport>;

/// Build the classifier for `[llm]` with its resolved API key (F-65).
///
/// The one place `run` and `doctor --online` construct it, so the engine's
/// liveness verdict and the operator's can never disagree about what they are
/// asking.
pub fn gateway_classifier(llm: &LlmConfig, api_key: SecretString) -> GatewayClassifier {
    let api_key = ApiKey::new(api_key.expose());
    let timeout = llm.timeout_secs.map(Duration::from_secs);
    match &llm.api {
        LlmApi::Chat {
            base_url,
            max_tokens,
        } => {
            let mut settings = ChatSettings::new(base_url, &llm.model, api_key);
            if let Some(timeout) = timeout {
                settings.timeout = timeout;
            }
            settings.max_tokens = *max_tokens;
            ConfiguredClassifier::Chat(ChatClassifier::new(ReqwestTransport::new(), settings))
        }
        LlmApi::Decisions { endpoint } => {
            let mut settings = DecisionsSettings::new(
                endpoint
                    .as_deref()
                    .unwrap_or(LlmApi::DEFAULT_DECISIONS_ENDPOINT),
                &llm.model,
                api_key,
            );
            if let Some(timeout) = timeout {
                settings.timeout = timeout;
            }
            ConfiguredClassifier::Decisions(DecisionsClassifier::new(
                ReqwestTransport::new(),
                settings,
            ))
        }
    }
}

/// What the engine currently knows about the LLM gateway (F-110 / F-111),
/// written by [`MonitoredClassifier`] on every call and read by `run` when it
/// publishes health.
///
/// Two latches and a timestamp:
///
/// - **`key_rejected`** — the gateway answered 401/403. A bad key does not
///   get better on its own, and its symptom is easy to misread: repository
///   selection simply falls back to asking the operator, which looks like a
///   slightly inconvenient normal day rather than a broken configuration.
/// - **`unreachable`** — the last call got no usable answer from the far side
///   ([`ClassifyError::is_unreachable`]: transport, timeout, 5xx). Carries a short
///   reason so the operator can tell "DNS" from "502" without opening the log.
/// - **`last_contact`** — when *any* call last completed, success or failure.
///   Real traffic is the best liveness check there is, so the engine only
///   spends a probe when this has gone quiet.
///
/// **Latches, not counters, and they clear themselves.** Any answer from the
/// gateway clears `unreachable`; any success clears `key_rejected`. So
/// rotating the key or the network coming back makes the warning disappear
/// on its own — the property that keeps the menu-bar glyph from becoming
/// permanent background noise. Each latch only listens to the errors that
/// say something about it: a timeout leaves `key_rejected` alone (it says
/// nothing about the key), and a 401 clears `unreachable` (the gateway is
/// evidently there).
///
/// Transitions are logged here, once per edge, rather than at every call
/// site: an outage that lasts an hour is one `warn` and one `info`, not a
/// line per probe.
#[derive(Debug, Default)]
pub struct LlmHealth {
    key_rejected: std::sync::atomic::AtomicBool,
    unreachable: std::sync::Mutex<Option<String>>,
    last_contact: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl LlmHealth {
    /// Whether the gateway rejected the configured credentials on the last
    /// call that answered.
    pub fn key_rejected(&self) -> bool {
        self.key_rejected.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Why the gateway is currently considered down, if it is.
    pub fn unreachable(&self) -> Option<String> {
        self.unreachable
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// When a call last completed (either way), or `None` if none has — or
    /// if [`forget_contact`](Self::forget_contact) was called since.
    pub fn last_contact(&self) -> Option<tokio::time::Instant> {
        *self.last_contact.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Discard the last-contact time so the next liveness check is due at
    /// once. The engine calls this on a resume from sleep: whatever was true
    /// before the machine slept is not evidence about now.
    pub fn forget_contact(&self) {
        *self.last_contact.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Fold one call's outcome into the latches.
    pub fn record<T>(&self, outcome: &Result<T, ClassifyError>) {
        use std::sync::atomic::Ordering::Relaxed;

        *self.last_contact.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(tokio::time::Instant::now());

        let reached = match outcome {
            Ok(_) => {
                if self.key_rejected.swap(false, Relaxed) {
                    tracing::info!("the LLM gateway accepted the API key again");
                }
                true
            }
            Err(e) if e.is_auth_failure() => {
                if !self.key_rejected.swap(true, Relaxed) {
                    // The short reason, not `%e`: a `Status` error carries the
                    // gateway's response body, which is the gateway's text to
                    // put anything in.
                    tracing::warn!(
                        reason = %short_reason(e),
                        "the LLM gateway rejected the API key → repository selection \
                         degrades until it is fixed; check `[llm].api_key_ref` and run \
                         `totsuka doctor --online`"
                    );
                }
                true
            }
            Err(e) if e.is_unreachable() => {
                let reason = short_reason(e);
                let mut slot = self.unreachable.lock().unwrap_or_else(|p| p.into_inner());
                if slot.is_none() {
                    tracing::warn!(
                        reason = %reason,
                        "the LLM gateway is not answering → tasks that need classification \
                         fail until it is back; check the network and `[llm].base_url` (chat) or `[llm].endpoint` (decisions)"
                    );
                }
                *slot = Some(reason);
                false
            }
            // A 4xx we do not treat as auth, or a response we could not
            // parse: the gateway answered. Says nothing about the key.
            Err(_) => true,
        };
        if reached {
            let mut slot = self.unreachable.lock().unwrap_or_else(|p| p.into_inner());
            if slot.take().is_some() {
                tracing::info!("the LLM gateway is answering again");
            }
        }
    }
}

/// The short, operator-facing reason stored in [`LlmHealth::unreachable`]
/// and written to the log on a key rejection.
///
/// Deliberately narrower than `Display` for [`ClassifyError`]: this string ends up
/// in `health.json` and on the `totsuka status` terminal, where the redacting
/// logging layer never sees it. A response body is dropped entirely — a
/// gateway echoing our request into its error page would land the credential
/// in a file — and a transport message is URL-scrubbed ([`scrub_urls`], a
/// second time, in case the error was built elsewhere) and truncated, since
/// reqwest's can nest the full URL chain.
fn short_reason(e: &ClassifyError) -> String {
    const MAX: usize = 160;
    match e {
        ClassifyError::Transport(msg) => {
            let msg = scrub_urls(msg);
            let mut s: String = msg.chars().take(MAX).collect();
            if msg.chars().count() > MAX {
                s.push('…');
            }
            format!("transport error: {s}")
        }
        ClassifyError::Timeout(secs) => format!("no answer within {secs}s"),
        ClassifyError::Status { status, .. } => format!("HTTP {status}"),
        ClassifyError::InvalidResponse(_) | ClassifyError::UnknownRepo(_) => {
            "unusable response".to_string()
        }
    }
}

/// A [`RepoClassifier`] decorator that keeps [`LlmHealth`] current (F-110 /
/// F-111).
///
/// Wrapping rather than checking at the call site is deliberate — every LLM
/// call the engine makes, present and future, goes through one place, and so
/// does every probe.
pub struct MonitoredClassifier<L> {
    inner: L,
    health: std::sync::Arc<LlmHealth>,
}

impl<L> MonitoredClassifier<L> {
    /// Wrap `inner`, reporting through the returned health record.
    pub fn new(inner: L) -> Self {
        Self {
            inner,
            health: std::sync::Arc::new(LlmHealth::default()),
        }
    }

    /// A handle on the health record, for whoever publishes it.
    pub fn health(&self) -> std::sync::Arc<LlmHealth> {
        std::sync::Arc::clone(&self.health)
    }
}

impl<L: RepoClassifier> RepoClassifier for MonitoredClassifier<L> {
    async fn classify(&self, request: &ClassifyRequest) -> Result<Classification, ClassifyError> {
        let result = self.inner.classify(request).await;
        self.health.record(&result);
        result
    }

    async fn probe(&self) -> Result<(), ClassifyError> {
        let before = self.health.last_contact();
        let result = self.inner.probe().await;
        // A real call that completed while this probe was in flight is newer
        // evidence than the probe; a probe that left before the gateway came
        // back must not report the outage that has since ended. Only one probe
        // runs at a time, so a changed `last_contact` can only mean traffic.
        if self.health.last_contact() == before {
            self.health.record(&result);
        } else {
            tracing::debug!(
                "llm probe finished after real traffic; its verdict is stale and dropped"
            );
        }
        result
    }

    fn reset_connections(&self) {
        self.inner.reset_connections();
    }
}

#[cfg(test)]
mod monitored_classifier_tests {
    use super::*;

    struct Scripted(std::sync::Mutex<Vec<Result<Classification, ClassifyError>>>);

    impl RepoClassifier for Scripted {
        async fn classify(
            &self,
            _request: &ClassifyRequest,
        ) -> Result<Classification, ClassifyError> {
            self.0.lock().unwrap().remove(0)
        }

        /// Probes are scripted from the same queue, mapped to `()`.
        async fn probe(&self) -> Result<(), ClassifyError> {
            self.0.lock().unwrap().remove(0).map(|_| ())
        }
    }

    fn request() -> ClassifyRequest {
        ClassifyRequest {
            subject: Vec::new(),
            candidates: Vec::new(),
            chat_prompt: None,
        }
    }

    fn verdict() -> Classification {
        Classification::Repo {
            repo: "r".into(),
            confidence: 1.0,
            reason: String::new(),
        }
    }

    fn scripted(
        script: Vec<Result<Classification, ClassifyError>>,
    ) -> MonitoredClassifier<Scripted> {
        MonitoredClassifier::new(Scripted(std::sync::Mutex::new(script)))
    }

    fn status(status: u16) -> ClassifyError {
        ClassifyError::status(status, "irrelevant")
    }

    #[tokio::test]
    async fn a_401_sets_the_key_latch_and_a_success_clears_it() {
        let router = scripted(vec![Err(status(401)), Ok(verdict())]);
        let health = router.health();
        assert!(!health.key_rejected(), "starts clear");

        let _ = router.classify(&request()).await;
        assert!(health.key_rejected(), "401 latches");

        let _ = router.classify(&request()).await;
        assert!(
            !health.key_rejected(),
            "a success clears it, so rotating the key makes the warning go away"
        );
    }

    /// The whole point of latching only on 401/403: an unrelated outage must
    /// not clear a real rejection, and must not raise one either.
    #[tokio::test]
    async fn outages_leave_the_key_latch_untouched() {
        let router = scripted(vec![
            Err(ClassifyError::Timeout(30)),
            Err(status(403)),
            Err(ClassifyError::Transport("refused".into())),
        ]);
        let health = router.health();

        let _ = router.classify(&request()).await;
        assert!(!health.key_rejected(), "a timeout raises nothing");

        let _ = router.classify(&request()).await;
        assert!(health.key_rejected(), "403 latches");

        let _ = router.classify(&request()).await;
        assert!(
            health.key_rejected(),
            "a later transport error must not clear a real rejection"
        );
    }

    /// Transport, timeout and 5xx all mean "not serving"; the reason names
    /// which, and never carries a response body.
    #[tokio::test]
    async fn not_serving_latches_unreachable_with_a_short_reason() {
        let router = scripted(vec![
            Err(ClassifyError::Transport("dns error: no such host".into())),
            Err(ClassifyError::Timeout(30)),
            Err(ClassifyError::status(
                502,
                "<html>sk-live-should-not-leak</html>",
            )),
        ]);
        let health = router.health();
        assert_eq!(health.unreachable(), None, "starts clear");

        let _ = router.classify(&request()).await;
        assert_eq!(
            health.unreachable().as_deref(),
            Some("transport error: dns error: no such host")
        );

        let _ = router.classify(&request()).await;
        assert_eq!(
            health.unreachable().as_deref(),
            Some("no answer within 30s")
        );

        let _ = router.classify(&request()).await;
        let reason = health.unreachable().expect("5xx latches");
        assert_eq!(reason, "HTTP 502");
        assert!(!reason.contains("sk-live"), "{reason}");
    }

    /// Any answer at all — a success, a 401, a 429, a 400, garbage — proves
    /// the gateway is there, so it clears the outage latch.
    #[tokio::test]
    async fn any_answer_clears_unreachable() {
        for answer in [
            Ok(verdict()),
            Err(status(401)),
            Err(status(429)),
            Err(status(400)),
            Err(ClassifyError::InvalidResponse("not json".into())),
        ] {
            let router = scripted(vec![Err(ClassifyError::Timeout(30)), answer]);
            let health = router.health();
            let _ = router.classify(&request()).await;
            assert!(health.unreachable().is_some(), "the outage latched first");
            let _ = router.classify(&request()).await;
            assert_eq!(health.unreachable(), None, "an answer clears it");
        }
    }

    /// A 429 is a gateway that is alive and busy — neither latch moves.
    #[tokio::test]
    async fn throttling_is_not_an_outage_and_not_a_bad_key() {
        let router = scripted(vec![Err(status(429))]);
        let health = router.health();
        let _ = router.classify(&request()).await;
        assert!(!health.key_rejected());
        assert_eq!(health.unreachable(), None);
    }

    /// Probes feed the same latches as real calls, and both count as
    /// contact — which is what lets the engine skip a probe while traffic is
    /// flowing.
    #[tokio::test]
    async fn probes_and_calls_both_count_as_contact() {
        let router = scripted(vec![
            Err(ClassifyError::Transport("refused".into())),
            Ok(verdict()),
        ]);
        let health = router.health();
        assert_eq!(health.last_contact(), None, "nothing has been asked yet");

        let _ = router.probe().await;
        assert!(health.unreachable().is_some(), "a failed probe latches");
        let first = health.last_contact().expect("a probe is contact");

        let _ = router.classify(&request()).await;
        assert_eq!(health.unreachable(), None, "a real call clears it");
        assert!(health.last_contact().expect("a call is contact") >= first);

        health.forget_contact();
        assert_eq!(health.last_contact(), None, "a resume forgets the past");
    }

    #[test]
    fn a_long_transport_message_is_truncated_for_the_health_file() {
        let long = "x".repeat(500);
        let reason = short_reason(&ClassifyError::Transport(long));
        assert!(reason.chars().count() < 200, "{}", reason.chars().count());
        assert!(reason.ends_with('…'));
    }

    /// A credential written into `base_url` must not travel into the health
    /// file through a transport error that echoes the URL.
    #[test]
    fn urls_in_transport_errors_lose_their_credentials_and_query() {
        let reason = short_reason(&ClassifyError::Transport(
            "https://u:p@h.example/v1?token=x failed".into(),
        ));
        assert_eq!(reason, "transport error: https://h.example/v1 failed");
    }

    /// A probe that was in flight while a real call succeeded is stale
    /// evidence: it must not re-latch an outage the traffic just disproved.
    #[tokio::test]
    async fn a_stale_probe_does_not_overwrite_newer_traffic() {
        struct Gated {
            release: tokio::sync::Notify,
        }
        impl RepoClassifier for Gated {
            async fn classify(
                &self,
                _request: &ClassifyRequest,
            ) -> Result<Classification, ClassifyError> {
                Ok(verdict())
            }
            async fn probe(&self) -> Result<(), ClassifyError> {
                self.release.notified().await;
                Err(ClassifyError::status(502, ""))
            }
        }
        let router = std::sync::Arc::new(MonitoredClassifier::new(Gated {
            release: tokio::sync::Notify::new(),
        }));
        let health = router.health();

        let probe = tokio::spawn({
            let router = std::sync::Arc::clone(&router);
            async move { router.probe().await }
        });
        tokio::task::yield_now().await;
        let _ = router.classify(&request()).await;
        assert!(
            health.last_contact().is_some(),
            "the real call was recorded"
        );

        router.inner.release.notify_one();
        assert!(
            probe.await.unwrap().is_err(),
            "the probe itself still failed"
        );
        assert_eq!(
            health.unreachable(),
            None,
            "…but its verdict was stale and did not latch"
        );
    }
}
