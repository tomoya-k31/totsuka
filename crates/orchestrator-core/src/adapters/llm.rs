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
    client: reqwest::Client,
    config: OpenAiConfig,
    api_key: SecretString,
}

impl OpenAiRouter {
    /// Build a router with a resolved API key (F-65).
    pub fn new(config: OpenAiConfig, api_key: SecretString) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
            api_key,
        }
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
            .client
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
            .client
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

/// An [`LlmRouter`] decorator that latches "the gateway rejected our
/// credentials" (F-110).
///
/// A bad key does not get better on its own, and its symptom is easy to
/// misread: repository selection simply falls back to asking the operator,
/// which looks like a slightly inconvenient normal day rather than a broken
/// configuration. So the fact is recorded where `run` can publish it as a
/// degradation.
///
/// **A latch, not a counter, and it clears itself.** Any successful call
/// resets it, so rotating the key makes the warning disappear on its own —
/// which is the property that keeps the menu-bar glyph from becoming
/// permanent background noise. Only 401/403 sets it
/// ([`LlmError::is_auth_failure`]): a timeout or a 5xx says nothing about
/// whether the key is good.
///
/// Wrapping rather than checking at the call site is deliberate — every LLM
/// call the engine makes, present and future, goes through one place.
pub struct AuthLatchRouter<L> {
    inner: L,
    rejected: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<L> AuthLatchRouter<L> {
    /// Wrap `inner`, reporting through the returned flag.
    pub fn new(inner: L) -> Self {
        Self {
            inner,
            rejected: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// A handle on the latch, for whoever publishes the health.
    pub fn flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.rejected)
    }
}

impl<L: LlmRouter> LlmRouter for AuthLatchRouter<L> {
    async fn chat_json(&self, request: &ChatRequest) -> Result<Value, LlmError> {
        let result = self.inner.chat_json(request).await;
        match &result {
            Ok(_) => self
                .rejected
                .store(false, std::sync::atomic::Ordering::Relaxed),
            Err(e) if e.is_auth_failure() => self
                .rejected
                .store(true, std::sync::atomic::Ordering::Relaxed),
            // Anything else says nothing about the key: leave the latch alone
            // rather than clearing a real rejection on an unrelated timeout.
            Err(_) => {}
        }
        result
    }
}

#[cfg(test)]
mod auth_latch_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    struct Scripted(std::sync::Mutex<Vec<Result<Value, LlmError>>>);

    impl LlmRouter for Scripted {
        async fn chat_json(&self, _request: &ChatRequest) -> Result<Value, LlmError> {
            self.0.lock().unwrap().remove(0)
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

    fn scripted(script: Vec<Result<Value, LlmError>>) -> AuthLatchRouter<Scripted> {
        AuthLatchRouter::new(Scripted(std::sync::Mutex::new(script)))
    }

    #[tokio::test]
    async fn a_401_sets_the_latch_and_a_success_clears_it() {
        let router = scripted(vec![
            Err(LlmError::Status {
                status: 401,
                body: "no".into(),
            }),
            Ok(json!({"ok": true})),
        ]);
        let flag = router.flag();
        assert!(!flag.load(Ordering::Relaxed), "starts clear");

        let _ = router.chat_json(&request()).await;
        assert!(flag.load(Ordering::Relaxed), "401 latches");

        let _ = router.chat_json(&request()).await;
        assert!(
            !flag.load(Ordering::Relaxed),
            "a success clears it, so rotating the key makes the warning go away"
        );
    }

    /// The whole point of latching only on 401/403: an unrelated outage must
    /// not clear a real rejection, and must not raise one either.
    #[tokio::test]
    async fn other_failures_leave_the_latch_untouched() {
        let router = scripted(vec![
            Err(LlmError::Timeout(30)),
            Err(LlmError::Status {
                status: 403,
                body: "nope".into(),
            }),
            Err(LlmError::Transport("refused".into())),
        ]);
        let flag = router.flag();

        let _ = router.chat_json(&request()).await;
        assert!(!flag.load(Ordering::Relaxed), "a timeout raises nothing");

        let _ = router.chat_json(&request()).await;
        assert!(flag.load(Ordering::Relaxed), "403 latches");

        let _ = router.chat_json(&request()).await;
        assert!(
            flag.load(Ordering::Relaxed),
            "a later transport error must not clear a real rejection"
        );
    }
}
