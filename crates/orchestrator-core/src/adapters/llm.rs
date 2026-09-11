//! OpenAI-compatible [`LlmRouter`] over `reqwest` (F-12, F-13, §5.3).
//!
//! Posts to `{base_url}/chat/completions` with `response_format: json_schema`
//! and returns the parsed structured content. Retryable failures (network,
//! timeout, 429/5xx) are retried with exponential backoff (§5.3).

use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::SecretString;
use crate::ports::llm::{ChatRequest, LlmError, LlmRouter};

/// Configuration for the OpenAI-compatible router.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL (e.g. `https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// Model name.
    pub model: String,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Number of retries for retryable failures (§5.3).
    pub max_retries: u32,
    /// Base backoff between retries (doubles each attempt).
    pub backoff_base: Duration,
}

impl OpenAiConfig {
    /// Sensible defaults for a base URL + model.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            timeout: Duration::from_secs(30),
            max_retries: 3,
            backoff_base: Duration::from_millis(500),
        }
    }
}

/// An OpenAI-compatible chat router.
pub struct OpenAiRouter {
    /// Behind a lock so [`reset_connections`](Self::reset_connections) can
    /// swap it for a fresh one; every request clones the handle out (a
    /// `reqwest::Client` is an `Arc` inside, so the clone is cheap) and never
    /// holds the lock across an await.
    client: std::sync::RwLock<reqwest::Client>,
    config: OpenAiConfig,
    api_key: SecretString,
}

impl OpenAiRouter {
    /// Build a router with a resolved API key (F-65).
    pub fn new(config: OpenAiConfig, api_key: SecretString) -> Self {
        Self {
            client: std::sync::RwLock::new(reqwest::Client::new()),
            config,
            api_key,
        }
    }

    /// The current HTTP client handle.
    fn client(&self) -> reqwest::Client {
        self.client
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Replace the HTTP client, abandoning its connection pool (F-111).
    ///
    /// After the machine sleeps, the pool's keep-alive connections are
    /// half-open: the peer has long since dropped them, but nothing told this
    /// side. The pool's own idle timeout does not save us — it is measured on
    /// the monotonic clock, which did not advance while the machine was
    /// asleep — so the first request after waking is spent discovering that
    /// the connection is dead (a fast reset if we are lucky, a full
    /// `timeout` if we are not). A new client starts with an empty pool and
    /// pays one TLS handshake instead. In-flight requests keep the old handle
    /// they cloned and finish on it.
    pub fn reset_connections(&self) {
        *self
            .client
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = reqwest::Client::new();
    }

    /// The `/chat/completions` URL.
    fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// One minimal live request asking only whether the gateway accepts our
    /// API key (`totsuka doctor --online`, #267).
    ///
    /// Deliberately not the [`LlmRouter`] path. No `response_format` schema:
    /// providers differ in what structured-output shapes they accept, and a
    /// rejected schema (400) would masquerade as a credentials problem. No
    /// retries: a probe answers now or not at all. `max_tokens: 1` so a
    /// healthy provider bills a rounding error. The response body is
    /// discarded — a 2xx has already proven the key was accepted, which is
    /// the entire question.
    pub async fn probe_auth(&self) -> Result<(), LlmError> {
        let body = json!({
            "model": self.config.model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
        });
        let response = self
            .client()
            .post(self.endpoint())
            .bearer_auth(self.api_key.expose())
            .timeout(self.config.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(self.config.timeout.as_secs())
                } else {
                    LlmError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // Body read failures must not mask the status we came for.
        let text = response.text().await.unwrap_or_default();
        Err(LlmError::Status {
            status: status.as_u16(),
            // Narrower than [`attempt`] deliberately: this body is *printed*
            // by `doctor` (stdout, not tracing), so the redacting logging
            // layer never sees it. A gateway that echoes the offending
            // credential in a 401 would land it on the operator's terminal
            // and in whatever they paste into an issue. An unrecognised
            // shape still falls back to the truncated raw body.
            body: error_message(&text).unwrap_or_else(|| text.chars().take(500).collect()),
        })
    }

    /// One request attempt, mapping transport/status errors to [`LlmError`].
    async fn attempt(&self, body: &Value) -> Result<Value, LlmError> {
        let response = self
            .client()
            .post(self.endpoint())
            .bearer_auth(self.api_key.expose())
            .timeout(self.config.timeout)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout(self.config.timeout.as_secs())
                } else {
                    LlmError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(LlmError::Status {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }

        parse_chat_content(&text)
    }
}

/// `error.message` out of an OpenAI-compatible error envelope, truncated.
/// `None` when the body is not that shape (HTML error page, bare text, a
/// proxy's own format), leaving the caller to decide on a fallback.
fn error_message(body: &str) -> Option<String> {
    let envelope: Value = serde_json::from_str(body).ok()?;
    Some(
        envelope
            .get("error")?
            .get("message")?
            .as_str()?
            .chars()
            .take(500)
            .collect(),
    )
}

/// Extract and parse `choices[0].message.content` (a JSON string, per structured
/// output) from a chat-completions response body.
fn parse_chat_content(text: &str) -> Result<Value, LlmError> {
    let envelope: Value =
        serde_json::from_str(text).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let content = envelope["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| LlmError::InvalidResponse("missing choices[0].message.content".into()))?;
    serde_json::from_str(content).map_err(|e| LlmError::InvalidResponse(e.to_string()))
}

impl LlmRouter for OpenAiRouter {
    fn chat_json(
        &self,
        request: &ChatRequest,
    ) -> impl std::future::Future<Output = Result<Value, LlmError>> + Send {
        let mut body = json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": request.system},
                {"role": "user", "content": request.user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "structured_output",
                    "schema": request.json_schema,
                    "strict": true,
                },
            },
        });
        // Only send `max_tokens` when set: OpenAI-compatible APIs often reject a
        // literal `null` for numeric fields with a 400.
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        async move {
            let mut attempt = 0;
            loop {
                match self.attempt(&body).await {
                    Ok(value) => return Ok(value),
                    Err(e) if e.is_retryable() && attempt < self.config.max_retries => {
                        // Cap the delay to avoid overflow panics on large attempt
                        // counts (saturating pow/mul, then a 60s ceiling).
                        let factor = 2u32.saturating_pow(attempt);
                        let delay = self
                            .config
                            .backoff_base
                            .saturating_mul(factor)
                            .min(Duration::from_secs(60));
                        tracing::warn!(attempt, error = %e, "llm call failed; retrying");
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    /// [`probe_auth`](Self::probe_auth): the same single unretried request
    /// `doctor --online` sends, so the engine's liveness verdict and the
    /// operator's can never disagree about what "alive" means.
    fn probe(&self) -> impl std::future::Future<Output = Result<(), LlmError>> + Send {
        self.probe_auth()
    }

    fn reset_connections(&self) {
        OpenAiRouter::reset_connections(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(content: &str) -> String {
        // `content` is embedded as a JSON string, as the API returns it.
        serde_json::json!({
            "choices": [{ "message": { "content": content } }]
        })
        .to_string()
    }

    #[test]
    fn parses_structured_content() {
        let body = envelope(r#"{"repo":"api","confidence":0.9,"reason":"backend"}"#);
        let value = parse_chat_content(&body).unwrap();
        assert_eq!(value["repo"], "api");
        assert_eq!(value["confidence"], 0.9);
    }

    #[test]
    fn non_json_content_is_invalid_response() {
        let body = envelope("I think it is the api repo");
        assert!(matches!(
            parse_chat_content(&body).unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
    }

    #[test]
    fn probe_error_body_keeps_only_the_provider_message() {
        assert_eq!(
            error_message(
                r#"{"error":{"message":"User not found.","code":401},"key":"sk-live-nope"}"#
            )
            .as_deref(),
            Some("User not found.")
        );
        // Shapes we do not recognise get no answer, so the caller falls back
        // to the raw body rather than swallowing the diagnosis.
        for body in [
            "<html>502</html>",
            r#"{"detail":"nope"}"#,
            r#"{"error":"bare string"}"#,
            "",
        ] {
            assert_eq!(error_message(body), None, "{body}");
        }
    }

    #[test]
    fn only_401_and_403_are_auth_failures() {
        let status = |status| LlmError::Status {
            status,
            body: String::new(),
        };
        assert!(status(401).is_auth_failure());
        assert!(status(403).is_auth_failure());
        // Busy, broken, or malformed — none of them say the key is bad.
        for other in [400, 404, 429, 500, 503] {
            assert!(!status(other).is_auth_failure(), "{other}");
        }
        assert!(!LlmError::Timeout(30).is_auth_failure());
        assert!(!LlmError::Transport("refused".into()).is_auth_failure());
        // An auth failure is terminal, never retried.
        assert!(!status(401).is_retryable());
    }

    #[test]
    fn missing_content_is_invalid_response() {
        let body = serde_json::json!({ "choices": [{ "message": {} }] }).to_string();
        assert!(matches!(
            parse_chat_content(&body).unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
        // Not even valid JSON.
        assert!(matches!(
            parse_chat_content("not json").unwrap_err(),
            LlmError::InvalidResponse(_)
        ));
    }
}

/// What the engine currently knows about the LLM gateway (F-110 / F-111),
/// written by [`LlmHealthRouter`] on every call and read by `run` when it
/// publishes health.
///
/// Two latches and a timestamp:
///
/// - **`key_rejected`** — the gateway answered 401/403. A bad key does not
///   get better on its own, and its symptom is easy to misread: repository
///   selection simply falls back to asking the operator, which looks like a
///   slightly inconvenient normal day rather than a broken configuration.
/// - **`unreachable`** — the last call got no usable answer from the far side
///   ([`LlmError::is_unreachable`]: transport, timeout, 5xx). Carries a short
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
    pub fn record<T>(&self, outcome: &Result<T, LlmError>) {
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
                    tracing::warn!(
                        error = %e,
                        "the LLM gateway rejected the API key → repository selection \
                         degrades until it is fixed; check `[llm].api_key_ref` and run \
                         `totsuka doctor --online`"
                    );
                }
                true
            }
            Err(e) if e.is_unreachable() => {
                let reason = unreachable_reason(e);
                let mut slot = self.unreachable.lock().unwrap_or_else(|p| p.into_inner());
                if slot.is_none() {
                    tracing::warn!(
                        reason = %reason,
                        "the LLM gateway is not answering → tasks that need classification \
                         fail until it is back; check the network and `[llm].base_url`"
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

/// The short, operator-facing reason stored in [`LlmHealth::unreachable`].
///
/// Deliberately narrower than `Display` for [`LlmError`]: this string ends up
/// in `health.json` and on the `totsuka status` terminal, where the redacting
/// logging layer never sees it. A 5xx body is dropped entirely — a gateway
/// echoing our request into its error page would land the credential in a
/// file — and a transport message is truncated, since reqwest's can nest the
/// full URL chain.
fn unreachable_reason(e: &LlmError) -> String {
    const MAX: usize = 160;
    match e {
        LlmError::Transport(msg) => {
            let mut s: String = msg.chars().take(MAX).collect();
            if msg.chars().count() > MAX {
                s.push('…');
            }
            format!("transport error: {s}")
        }
        LlmError::Timeout(secs) => format!("no answer within {secs}s"),
        LlmError::Status { status, .. } => format!("HTTP {status}"),
        LlmError::InvalidResponse(_) => "unusable response".to_string(),
    }
}

/// An [`LlmRouter`] decorator that keeps [`LlmHealth`] current (F-110 /
/// F-111).
///
/// Wrapping rather than checking at the call site is deliberate — every LLM
/// call the engine makes, present and future, goes through one place, and so
/// does every probe.
pub struct LlmHealthRouter<L> {
    inner: L,
    health: std::sync::Arc<LlmHealth>,
}

impl<L> LlmHealthRouter<L> {
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

impl<L: LlmRouter> LlmRouter for LlmHealthRouter<L> {
    async fn chat_json(&self, request: &ChatRequest) -> Result<Value, LlmError> {
        let result = self.inner.chat_json(request).await;
        self.health.record(&result);
        result
    }

    async fn probe(&self) -> Result<(), LlmError> {
        let result = self.inner.probe().await;
        self.health.record(&result);
        result
    }

    fn reset_connections(&self) {
        self.inner.reset_connections();
    }
}

#[cfg(test)]
mod health_router_tests {
    use super::*;

    struct Scripted(std::sync::Mutex<Vec<Result<Value, LlmError>>>);

    impl LlmRouter for Scripted {
        async fn chat_json(&self, _request: &ChatRequest) -> Result<Value, LlmError> {
            self.0.lock().unwrap().remove(0)
        }

        /// Probes are scripted from the same queue, mapped to `()`.
        async fn probe(&self) -> Result<(), LlmError> {
            self.0.lock().unwrap().remove(0).map(|_| ())
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            system: String::new(),
            user: String::new(),
            json_schema: json!({}),
            max_tokens: None,
        }
    }

    fn scripted(script: Vec<Result<Value, LlmError>>) -> LlmHealthRouter<Scripted> {
        LlmHealthRouter::new(Scripted(std::sync::Mutex::new(script)))
    }

    fn status(status: u16) -> LlmError {
        LlmError::Status {
            status,
            body: "irrelevant".into(),
        }
    }

    #[tokio::test]
    async fn a_401_sets_the_key_latch_and_a_success_clears_it() {
        let router = scripted(vec![Err(status(401)), Ok(json!({"ok": true}))]);
        let health = router.health();
        assert!(!health.key_rejected(), "starts clear");

        let _ = router.chat_json(&request()).await;
        assert!(health.key_rejected(), "401 latches");

        let _ = router.chat_json(&request()).await;
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
            Err(LlmError::Timeout(30)),
            Err(status(403)),
            Err(LlmError::Transport("refused".into())),
        ]);
        let health = router.health();

        let _ = router.chat_json(&request()).await;
        assert!(!health.key_rejected(), "a timeout raises nothing");

        let _ = router.chat_json(&request()).await;
        assert!(health.key_rejected(), "403 latches");

        let _ = router.chat_json(&request()).await;
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
            Err(LlmError::Transport("dns error: no such host".into())),
            Err(LlmError::Timeout(30)),
            Err(LlmError::Status {
                status: 502,
                body: "<html>sk-live-should-not-leak</html>".into(),
            }),
        ]);
        let health = router.health();
        assert_eq!(health.unreachable(), None, "starts clear");

        let _ = router.chat_json(&request()).await;
        assert_eq!(
            health.unreachable().as_deref(),
            Some("transport error: dns error: no such host")
        );

        let _ = router.chat_json(&request()).await;
        assert_eq!(
            health.unreachable().as_deref(),
            Some("no answer within 30s")
        );

        let _ = router.chat_json(&request()).await;
        let reason = health.unreachable().expect("5xx latches");
        assert_eq!(reason, "HTTP 502");
        assert!(!reason.contains("sk-live"), "{reason}");
    }

    /// Any answer at all — a success, a 401, a 429, a 400, garbage — proves
    /// the gateway is there, so it clears the outage latch.
    #[tokio::test]
    async fn any_answer_clears_unreachable() {
        for answer in [
            Ok(json!({"ok": true})),
            Err(status(401)),
            Err(status(429)),
            Err(status(400)),
            Err(LlmError::InvalidResponse("not json".into())),
        ] {
            let router = scripted(vec![Err(LlmError::Timeout(30)), answer]);
            let health = router.health();
            let _ = router.chat_json(&request()).await;
            assert!(health.unreachable().is_some(), "the outage latched first");
            let _ = router.chat_json(&request()).await;
            assert_eq!(health.unreachable(), None, "an answer clears it");
        }
    }

    /// A 429 is a gateway that is alive and busy — neither latch moves.
    #[tokio::test]
    async fn throttling_is_not_an_outage_and_not_a_bad_key() {
        let router = scripted(vec![Err(status(429))]);
        let health = router.health();
        let _ = router.chat_json(&request()).await;
        assert!(!health.key_rejected());
        assert_eq!(health.unreachable(), None);
    }

    /// Probes feed the same latches as real calls, and both count as
    /// contact — which is what lets the engine skip a probe while traffic is
    /// flowing.
    #[tokio::test]
    async fn probes_and_calls_both_count_as_contact() {
        let router = scripted(vec![
            Err(LlmError::Transport("refused".into())),
            Ok(json!({"ok": true})),
        ]);
        let health = router.health();
        assert_eq!(health.last_contact(), None, "nothing has been asked yet");

        let _ = router.probe().await;
        assert!(health.unreachable().is_some(), "a failed probe latches");
        let first = health.last_contact().expect("a probe is contact");

        let _ = router.chat_json(&request()).await;
        assert_eq!(health.unreachable(), None, "a real call clears it");
        assert!(health.last_contact().expect("a call is contact") >= first);

        health.forget_contact();
        assert_eq!(health.last_contact(), None, "a resume forgets the past");
    }

    #[test]
    fn a_long_transport_message_is_truncated_for_the_health_file() {
        let long = "x".repeat(500);
        let reason = unreachable_reason(&LlmError::Transport(long));
        assert!(reason.chars().count() < 200, "{}", reason.chars().count());
        assert!(reason.ends_with('…'));
    }

    /// The real router must survive a reset mid-life: the next request simply
    /// uses the new client. Exercised against a closed port so no network is
    /// needed and the outcome is the same before and after.
    #[tokio::test]
    async fn reset_connections_leaves_the_router_usable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = OpenAiConfig::new(format!("http://127.0.0.1:{port}/v1"), "m");
        config.timeout = Duration::from_secs(2);
        let router = OpenAiRouter::new(config, SecretString::new(""));

        let before = router.probe().await.unwrap_err();
        router.reset_connections();
        let after = router.probe().await.unwrap_err();
        assert!(before.is_unreachable(), "{before}");
        assert!(after.is_unreachable(), "{after}");
    }
}
