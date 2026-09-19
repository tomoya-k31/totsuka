//! The plugin's own repository classification: which candidate repository a
//! mention concerns. Independent of the orchestrator's `[llm]` — resolution
//! happens entirely inside this plugin so the submitted task always carries a
//! final `repo_hint` (F-10 decides instantly, never falling back to core
//! selection).
//!
//! The classifier itself is the shared `repo-classifier` crate. What stays
//! here is Slack's side of it: the configurable prompt templates, reading
//! candidate READMEs, and the confidence threshold.

use repo_classifier::{
    ApiKey, Candidate, ChatClassifier, ChatOutput, ChatPrompt, ChatSettings, Classification,
    ClassifyRequest, HttpTransport, RepoClassifier, RetryPolicy,
};

use crate::config::{LlmConfig, RepoInfo, SlackPrompts};
use crate::template;

/// How much of a candidate's README is offered to the classifier.
const README_HEAD_LINES: usize = 30;

/// How long one classification request may take.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Why classification did not produce a usable verdict; every variant falls
/// through to the ephemeral picker (stage ③).
#[derive(Debug, thiserror::Error)]
pub enum ClassifyError {
    /// The HTTP call failed (network, non-2xx, timeout).
    #[error("LLM request failed: {0}")]
    Request(repo_classifier::ClassifyError),
    /// The response did not contain a readable verdict, even after the
    /// correction retry.
    #[error("LLM returned an unusable response: {0}")]
    InvalidResponse(String),
    /// The verdict named a repository that is not among the candidates.
    #[error("LLM chose `{0}`, which is not a candidate")]
    UnknownRepo(String),
    /// Valid verdict, but below the configured confidence threshold.
    #[error("LLM confidence {confidence:.2} is below the threshold {threshold:.2}")]
    LowConfidence {
        /// The verdict's confidence.
        confidence: f64,
        /// The configured minimum.
        threshold: f64,
    },
}

impl ClassifyError {
    /// Whether the provider rejected the API key (HTTP 401/403).
    ///
    /// Every variant degrades to the same picker, but this one is not an
    /// inconclusive answer — it is a broken configuration that will keep
    /// failing (and keep costing a round-trip per mention) until the key is
    /// fixed, so callers surface it louder (#267).
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, ClassifyError::Request(e) if e.is_auth_failure())
    }
}

impl From<repo_classifier::ClassifyError> for ClassifyError {
    fn from(e: repo_classifier::ClassifyError) -> Self {
        match e {
            repo_classifier::ClassifyError::InvalidResponse(why) => Self::InvalidResponse(why),
            repo_classifier::ClassifyError::UnknownRepo(repo) => Self::UnknownRepo(repo),
            request => Self::Request(request),
        }
    }
}

/// Classify which of `candidates` the mention concerns.
///
/// An unreadable verdict is retried once with a correction — the malformed
/// answer echoed back and [`SlackPrompts::classifier_correction`] appended —
/// since resending an identical body at temperature 0 to a deterministic
/// provider would only repeat the failure. Any terminal failure is a
/// [`ClassifyError`] and the caller falls through to the ephemeral picker.
pub async fn classify<T: HttpTransport>(
    transport: &T,
    config: &LlmConfig,
    prompts: &SlackPrompts,
    mention_text: &str,
    thread_context: &str,
    candidates: &[RepoInfo],
) -> Result<Classification, ClassifyError> {
    // README reads are blocking filesystem I/O; keep them off the async
    // worker so a slow disk cannot stall the runtime.
    let request = {
        let prompts = prompts.clone();
        let mention = mention_text.to_string();
        let context = thread_context.to_string();
        let candidates = candidates.to_vec();
        tokio::task::spawn_blocking(move || {
            classify_request(&prompts, &mention, &context, &candidates)
        })
        .await
        .map_err(|e| {
            ClassifyError::Request(repo_classifier::ClassifyError::Transport(format!(
                "request build failed: {e}"
            )))
        })?
    };

    let verdict = ChatClassifier::new(transport, chat_settings(config))
        .classify(&request)
        .await?;
    if verdict.confidence < config.confidence_threshold {
        return Err(ClassifyError::LowConfidence {
            confidence: verdict.confidence,
            threshold: config.confidence_threshold,
        });
    }
    Ok(verdict)
}

/// The plugin's call shape: the prompt asks for JSON in prose (no
/// `response_format`, which not every model accepts), temperature 0, one
/// attempt with a generous timeout.
fn chat_settings(config: &LlmConfig) -> ChatSettings {
    let mut settings = ChatSettings::new(
        &config.base_url,
        &config.model,
        ApiKey::new(config.api_key.as_str()),
    );
    settings.output = ChatOutput::Prose;
    settings.temperature = Some(0.0);
    settings.timeout = REQUEST_TIMEOUT;
    settings.retry = RetryPolicy::NONE;
    settings
}

/// The classification question: the mention and its thread as the subject,
/// each candidate with its summary and README head, and the operator's
/// templates rendered into the chat prompt.
fn classify_request(
    prompts: &SlackPrompts,
    mention_text: &str,
    thread_context: &str,
    repos: &[RepoInfo],
) -> ClassifyRequest {
    let candidates: Vec<Candidate> = repos
        .iter()
        .map(|repo| Candidate {
            name: repo.name.clone(),
            summary: repo.summary.clone(),
            readme_head: repo.path.as_deref().and_then(readme_head),
        })
        .collect();

    let mut catalog = String::new();
    for c in &candidates {
        catalog.push_str(&format!("### {}\n", c.name));
        if let Some(summary) = &c.summary {
            catalog.push_str(&format!("summary: {summary}\n"));
        }
        if let Some(head) = &c.readme_head {
            catalog.push_str(&format!("README (head):\n{head}\n"));
        }
        catalog.push('\n');
    }

    let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
    let system = template::render(
        &prompts.classifier_system,
        &[("repo_names", names.join(", ").as_str())],
    );
    // Every one of these is Slack content the message author chose. Rendering
    // is single-pass, so a mention containing the literal text `{catalog}` is
    // inserted rather than splicing the candidate list in.
    let user = template::render(
        &prompts.classifier_user,
        &[
            ("mention_text", mention_text),
            ("thread_context", thread_context),
            ("catalog", catalog.as_str()),
        ],
    );

    let mut subject = vec![("Mention".to_string(), mention_text.to_string())];
    if !thread_context.is_empty() {
        subject.push(("Thread context".to_string(), thread_context.to_string()));
    }
    ClassifyRequest {
        subject,
        candidates,
        chat_prompt: Some(ChatPrompt {
            system,
            user,
            correction: Some(prompts.classifier_correction.clone()),
        }),
    }
}

/// The first `README_HEAD_LINES` lines of `{path}/README.md`; `None` when
/// unreadable (missing file is normal, not an error).
fn readme_head(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(std::path::Path::new(path).join("README.md")).ok()?;
    Some(
        text.lines()
            .take(README_HEAD_LINES)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use repo_classifier::HttpRequest;
    use serde_json::{Value, json};

    type WireError = repo_classifier::ClassifyError;

    fn repo(name: &str) -> RepoInfo {
        RepoInfo {
            name: name.to_string(),
            summary: Some(format!("{name} summary")),
            path: None,
        }
    }

    fn config() -> LlmConfig {
        LlmConfig {
            base_url: "https://llm.test/v1".into(),
            model: "test-model".into(),
            api_key: "sk-test".into(),
            confidence_threshold: 0.6,
        }
    }

    fn prompts() -> SlackPrompts {
        SlackPrompts::default()
    }

    fn chat_response(content: &str) -> Value {
        json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
    }

    /// A transport answering from a queue; records URLs and request bodies.
    struct FakeChat {
        responses: std::sync::Mutex<std::collections::VecDeque<Result<Value, WireError>>>,
        requests: std::sync::Mutex<Vec<(String, Value)>>,
    }

    impl FakeChat {
        fn new(responses: Vec<Result<Value, WireError>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn bodies(&self) -> Vec<Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|(_, body)| body.clone())
                .collect()
        }
    }

    impl HttpTransport for FakeChat {
        async fn post_json(&self, request: HttpRequest<'_>) -> Result<Value, WireError> {
            self.requests
                .lock()
                .unwrap()
                .push((request.url.to_string(), request.body.clone()));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(WireError::Transport("no canned response".into())))
        }
    }

    async fn run(
        chat: &FakeChat,
        candidates: &[RepoInfo],
    ) -> Result<Classification, ClassifyError> {
        classify(chat, &config(), &prompts(), "m", "", candidates).await
    }

    #[tokio::test]
    async fn confident_verdict_resolves() {
        let candidates = [repo("web-app"), repo("design-system")];
        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "web-app", "confidence": 0.9, "reason": "frontend bug"}"#,
        ))]);

        let verdict = classify(
            &chat,
            &config(),
            &prompts(),
            "the button is broken",
            "",
            &candidates,
        )
        .await
        .unwrap();
        assert_eq!(verdict.repo, "web-app");

        // The request carried the model, the contract, and the candidates.
        let requests = chat.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let (url, body) = &requests[0];
        assert_eq!(url, "https://llm.test/v1/chat/completions");
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["temperature"], 0.0);
        assert!(
            body.get("response_format").is_none(),
            "the plugin asks for JSON in prose, not via a schema"
        );
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("web-app, design-system"), "{system}");
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("the button is broken"), "{user}");
        assert!(user.contains("web-app summary"), "{user}");
    }

    #[tokio::test]
    async fn code_fenced_verdict_is_tolerated() {
        let chat = FakeChat::new(vec![Ok(chat_response(
            "Sure! Here you go:\n```json\n{\"repo\": \"web-app\", \"confidence\": 0.8, \
             \"reason\": \"x\"}\n```",
        ))]);
        let verdict = run(&chat, &[repo("web-app")]).await.unwrap();
        assert_eq!(verdict.repo, "web-app");
    }

    #[tokio::test]
    async fn low_confidence_is_reported_as_such() {
        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "web-app", "confidence": 0.3, "reason": "unsure"}"#,
        ))]);
        let err = run(&chat, &[repo("web-app")]).await.unwrap_err();
        assert!(matches!(
            err,
            ClassifyError::LowConfidence { confidence, .. } if (confidence - 0.3).abs() < 1e-9
        ));
    }

    #[tokio::test]
    async fn malformed_verdict_retries_once_then_fails() {
        let chat = FakeChat::new(vec![
            Ok(chat_response("I think it's the web app.")),
            Ok(chat_response("still prose")),
        ]);
        let err = run(&chat, &[repo("web-app")]).await.unwrap_err();
        assert!(matches!(err, ClassifyError::InvalidResponse(_)), "{err}");
        assert_eq!(chat.bodies().len(), 2, "exactly one retry");
    }

    #[tokio::test]
    async fn malformed_then_valid_verdict_succeeds_on_retry() {
        let chat = FakeChat::new(vec![
            Ok(chat_response("prose")),
            Ok(chat_response(
                r#"{"repo": "web-app", "confidence": 0.9, "reason": "ok"}"#,
            )),
        ]);
        let verdict = run(&chat, &[repo("web-app")]).await.unwrap();
        assert_eq!(verdict.repo, "web-app");
    }

    #[tokio::test]
    async fn unknown_repo_and_api_failure_fall_through() {
        let candidates = [repo("web-app")];

        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "ghost", "confidence": 0.9, "reason": "x"}"#,
        ))]);
        let err = run(&chat, &candidates).await.unwrap_err();
        assert!(
            matches!(err, ClassifyError::UnknownRepo(ref r) if r == "ghost"),
            "{err}"
        );

        let chat = FakeChat::new(vec![Err(WireError::Transport("connection refused".into()))]);
        let err = run(&chat, &candidates).await.unwrap_err();
        assert!(matches!(err, ClassifyError::Request(_)), "{err}");
        // A transport failure carries no status, so it is not an auth failure.
        assert!(!err.is_auth_failure(), "{err}");
        assert_eq!(
            chat.bodies().len(),
            1,
            "the plugin does not retry transport failures"
        );
    }

    #[tokio::test]
    async fn rejected_api_key_is_flagged_as_an_auth_failure() {
        for status in [401, 403] {
            let chat = FakeChat::new(vec![Err(WireError::status(
                status,
                r#"{"error":{"message":"User not found.","code":401},"request":{"api_key":"sk-live-should-not-be-logged"}}"#,
            ))]);
            let err = run(&chat, &[repo("web-app")]).await.unwrap_err();
            assert!(err.is_auth_failure(), "{status}: {err}");
            // The provider's sentence survives into the message the operator
            // reads; the rest of the body — where a gateway may echo the
            // credential — does not, since the plugin has no redacting layer.
            assert!(err.to_string().contains("User not found."), "{err}");
            assert!(!err.to_string().contains("sk-live"), "{err}");
        }
    }

    #[tokio::test]
    async fn other_http_failures_are_not_auth_failures() {
        // 429 and 5xx are the provider being busy or broken, not a bad key.
        for status in [429, 500, 503] {
            let chat = FakeChat::new(vec![Err(WireError::status(status, "busy"))]);
            let err = run(&chat, &[repo("web-app")]).await.unwrap_err();
            assert!(!err.is_auth_failure(), "{status}: {err}");
        }
        // Neither is a verdict we simply could not use.
        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "web-app", "confidence": 0.1, "reason": "unsure"}"#,
        ))]);
        let err = run(&chat, &[repo("web-app")]).await.unwrap_err();
        assert!(!err.is_auth_failure(), "{err}");
    }

    #[tokio::test]
    async fn retry_carries_a_correction_message() {
        let chat = FakeChat::new(vec![
            Ok(chat_response("just prose")),
            Ok(chat_response(
                r#"{"repo": "web-app", "confidence": 0.9, "reason": "ok"}"#,
            )),
        ]);
        run(&chat, &[repo("web-app")]).await.unwrap();

        let bodies = chat.bodies();
        assert_eq!(bodies.len(), 2);
        // The retry is not a byte-identical resend: it echoes the malformed
        // answer and appends the corrective instruction.
        let retry_messages = bodies[1]["messages"].as_array().unwrap();
        assert_eq!(retry_messages.len(), 4, "{retry_messages:?}");
        assert_eq!(retry_messages[2]["role"], "assistant");
        assert_eq!(retry_messages[2]["content"], "just prose");
        assert!(
            retry_messages[3]["content"]
                .as_str()
                .unwrap()
                .contains("ONLY the JSON object")
        );
    }

    #[test]
    fn the_request_carries_the_mention_and_every_candidate() {
        let request = classify_request(
            &prompts(),
            "the button is broken",
            "earlier: it was fine yesterday",
            &[repo("web-app"), repo("design-system")],
        );
        assert_eq!(
            request.subject,
            vec![
                ("Mention".to_string(), "the button is broken".to_string()),
                (
                    "Thread context".to_string(),
                    "earlier: it was fine yesterday".to_string()
                ),
            ]
        );
        let names: Vec<&str> = request.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["web-app", "design-system"]);
        let prompt = request
            .chat_prompt
            .expect("the plugin renders its own prompt");
        assert!(prompt.user.contains("### design-system"), "{}", prompt.user);
        assert!(prompt.correction.is_some());
    }
}
