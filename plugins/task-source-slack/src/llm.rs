//! The plugin's own repository classifier: one OpenAI-compatible
//! `/chat/completions` call deciding which candidate repository a mention
//! concerns. Independent of the orchestrator's `[llm]` — resolution happens
//! entirely inside this plugin so the submitted task always carries a final
//! `repo_hint` (F-10 decides instantly, never falling back to core LLM
//! selection).

use std::future::Future;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{LlmConfig, RepoInfo};

/// How much of a candidate's README is offered to the classifier.
const README_HEAD_LINES: usize = 30;

/// A classification verdict from the LLM.
#[derive(Debug, Clone, Deserialize)]
pub struct Classification {
    /// The chosen repository name (must be one of the candidates).
    pub repo: String,
    /// The model's self-reported confidence, 0.0–1.0.
    pub confidence: f64,
    /// One-line reasoning (logged, not shown to Slack).
    #[serde(default)]
    pub reason: String,
}

/// Why classification did not produce a usable verdict; every variant falls
/// through to the ephemeral picker (stage ③).
#[derive(Debug, thiserror::Error)]
pub enum ClassifyError {
    /// The HTTP call failed (network, non-2xx, timeout).
    #[error("LLM request failed: {0}")]
    Request(String),
    /// The response did not contain the JSON verdict we asked for, even
    /// after the retry.
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

/// Sends one chat-completion request. Seam for tests; production is
/// [`ReqwestChat`].
pub trait ChatTransport: Send + Sync {
    /// POST `body` to `{base_url}/chat/completions` with the API key,
    /// returning the parsed response JSON.
    fn complete(
        &self,
        config: &LlmConfig,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send;
}

/// Production transport over reqwest.
pub struct ReqwestChat {
    client: reqwest::Client,
}

impl ReqwestChat {
    /// A transport with its own connection pool.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestChat {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatTransport for ReqwestChat {
    async fn complete(&self, config: &LlmConfig, body: Value) -> Result<Value, String> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .bearer_auth(&config.api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let text = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                text.chars().take(300).collect::<String>()
            ));
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

/// Classify which of `candidates` the mention concerns. One retry on a
/// malformed verdict — with the malformed answer echoed back and a
/// corrective instruction appended, since resending an identical body at
/// temperature 0 to a deterministic provider would only repeat the failure.
/// Any terminal failure is a [`ClassifyError`] and the caller falls through
/// to the ephemeral picker.
pub async fn classify<C: ChatTransport>(
    chat: &C,
    config: &LlmConfig,
    mention_text: &str,
    thread_context: &str,
    candidates: &[RepoInfo],
) -> Result<Classification, ClassifyError> {
    // README reads are blocking filesystem I/O; keep them off the async
    // worker so a slow disk cannot stall the runtime.
    let body = {
        let config = config.clone();
        let mention = mention_text.to_string();
        let context = thread_context.to_string();
        let candidates = candidates.to_vec();
        tokio::task::spawn_blocking(move || request_body(&config, &mention, &context, &candidates))
            .await
            .map_err(|e| ClassifyError::Request(format!("request build failed: {e}")))?
    };

    let response = chat
        .complete(config, body.clone())
        .await
        .map_err(ClassifyError::Request)?;
    let (content, first_error) = match parse_verdict(&response) {
        Ok(verdict) => return validate(verdict, config, candidates),
        Err((content, error)) => (content, error),
    };

    tracing::info!(
        error = first_error,
        "LLM verdict malformed; retrying with a correction"
    );
    let retry_body = with_correction(body, &content);
    let response = chat
        .complete(config, retry_body)
        .await
        .map_err(ClassifyError::Request)?;
    match parse_verdict(&response) {
        Ok(verdict) => validate(verdict, config, candidates),
        Err((_, error)) => Err(ClassifyError::InvalidResponse(error)),
    }
}

/// The retry request: the original conversation plus the model's malformed
/// answer and an instruction to answer again with only the JSON object.
fn with_correction(mut body: Value, previous_answer: &str) -> Value {
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        messages.push(json!({ "role": "assistant", "content": previous_answer }));
        messages.push(json!({
            "role": "user",
            "content": "That response was not a parseable JSON verdict. Answer again with \
                        ONLY the JSON object — no prose, no code fences.",
        }));
    }
    body
}

/// Check the verdict names a real candidate and clears the threshold.
fn validate(
    verdict: Classification,
    config: &LlmConfig,
    candidates: &[RepoInfo],
) -> Result<Classification, ClassifyError> {
    if !candidates.iter().any(|r| r.name == verdict.repo) {
        return Err(ClassifyError::UnknownRepo(verdict.repo));
    }
    if verdict.confidence < config.confidence_threshold {
        return Err(ClassifyError::LowConfidence {
            confidence: verdict.confidence,
            threshold: config.confidence_threshold,
        });
    }
    Ok(verdict)
}

/// Build the chat-completion request: a system prompt fixing the JSON output
/// contract, and a user message carrying the mention, context, and candidate
/// material (summary + README head when a path is configured).
fn request_body(
    config: &LlmConfig,
    mention_text: &str,
    thread_context: &str,
    candidates: &[RepoInfo],
) -> Value {
    let mut catalog = String::new();
    for repo in candidates {
        catalog.push_str(&format!("### {}\n", repo.name));
        if let Some(summary) = &repo.summary {
            catalog.push_str(&format!("summary: {summary}\n"));
        }
        if let Some(head) = repo.path.as_deref().and_then(readme_head) {
            catalog.push_str(&format!("README (head):\n{head}\n"));
        }
        catalog.push('\n');
    }

    let names: Vec<&str> = candidates.iter().map(|r| r.name.as_str()).collect();
    let system = format!(
        "You classify which local repository a Slack mention is about. \
         Answer with ONLY a JSON object of the exact shape \
         {{\"repo\": string, \"confidence\": number, \"reason\": string}} — \
         no prose, no code fences. `repo` MUST be one of: {}. `confidence` \
         is 0.0-1.0, your own estimate of how sure you are.",
        names.join(", ")
    );
    let user = format!(
        "## Mention\n{mention_text}\n\n## Thread context\n{thread_context}\n\n\
         ## Candidate repositories\n{catalog}"
    );

    json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": 0,
    })
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

/// Extract the verdict object from a chat-completion response, tolerating
/// code fences and surrounding prose (models add them despite instructions).
/// The error side carries the raw content so the retry can echo it back.
fn parse_verdict(response: &Value) -> Result<Classification, (String, String)> {
    let Some(content) = response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
    else {
        return Err((
            String::new(),
            "response has no choices[0].message.content".to_string(),
        ));
    };
    extract_verdict(content).ok_or_else(|| {
        (
            content.to_string(),
            format!("no JSON verdict in content: {content:.200}"),
        )
    })
}

/// Try every `{` in `text` as the start of a balanced JSON object and return
/// the first one that deserializes as a [`Classification`]. Anchoring on the
/// *first* brace only would let prose like `the {Button} component` shadow a
/// perfectly valid verdict later in the answer.
fn extract_verdict(text: &str) -> Option<Classification> {
    for (start, _) in text.char_indices().filter(|(_, c)| *c == '{') {
        if let Some(candidate) = balanced_object(&text[start..])
            && let Ok(verdict) = serde_json::from_str::<Classification>(candidate)
        {
            return Some(verdict);
        }
    }
    None
}

/// The balanced `{…}` block at the start of `text`, if any.
fn balanced_object(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn chat_response(content: &str) -> Value {
        json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
    }

    /// A ChatTransport answering from a queue; records request bodies.
    struct FakeChat {
        responses: std::sync::Mutex<std::collections::VecDeque<Result<Value, String>>>,
        requests: std::sync::Mutex<Vec<Value>>,
    }

    impl FakeChat {
        fn new(responses: Vec<Result<Value, String>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatTransport for FakeChat {
        async fn complete(&self, _config: &LlmConfig, body: Value) -> Result<Value, String> {
            self.requests.lock().unwrap().push(body);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("no canned response".into()))
        }
    }

    #[tokio::test]
    async fn confident_verdict_resolves() {
        let candidates = [repo("web-app"), repo("design-system")];
        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "web-app", "confidence": 0.9, "reason": "frontend bug"}"#,
        ))]);

        let verdict = classify(&chat, &config(), "the button is broken", "", &candidates)
            .await
            .unwrap();
        assert_eq!(verdict.repo, "web-app");

        // The request carried the model, the contract, and the candidates.
        let requests = chat.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["model"], "test-model");
        assert_eq!(requests[0]["temperature"], 0);
        let system = requests[0]["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("web-app, design-system"), "{system}");
        let user = requests[0]["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("the button is broken"), "{user}");
        assert!(user.contains("web-app summary"), "{user}");
    }

    #[tokio::test]
    async fn code_fenced_verdict_is_tolerated() {
        let candidates = [repo("web-app")];
        let chat = FakeChat::new(vec![Ok(chat_response(
            "Sure! Here you go:\n```json\n{\"repo\": \"web-app\", \"confidence\": 0.8, \
             \"reason\": \"x\"}\n```",
        ))]);
        let verdict = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap();
        assert_eq!(verdict.repo, "web-app");
    }

    #[tokio::test]
    async fn low_confidence_is_reported_as_such() {
        let candidates = [repo("web-app")];
        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "web-app", "confidence": 0.3, "reason": "unsure"}"#,
        ))]);
        let err = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ClassifyError::LowConfidence { confidence, .. } if (confidence - 0.3).abs() < 1e-9
        ));
    }

    #[tokio::test]
    async fn malformed_verdict_retries_once_then_fails() {
        let candidates = [repo("web-app")];
        let chat = FakeChat::new(vec![
            Ok(chat_response("I think it's the web app.")),
            Ok(chat_response("still prose")),
        ]);
        let err = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap_err();
        assert!(matches!(err, ClassifyError::InvalidResponse(_)), "{err}");
        assert_eq!(chat.requests.lock().unwrap().len(), 2, "exactly one retry");
    }

    #[tokio::test]
    async fn malformed_then_valid_verdict_succeeds_on_retry() {
        let candidates = [repo("web-app")];
        let chat = FakeChat::new(vec![
            Ok(chat_response("prose")),
            Ok(chat_response(
                r#"{"repo": "web-app", "confidence": 0.9, "reason": "ok"}"#,
            )),
        ]);
        let verdict = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap();
        assert_eq!(verdict.repo, "web-app");
    }

    #[tokio::test]
    async fn unknown_repo_and_api_failure_fall_through() {
        let candidates = [repo("web-app")];

        let chat = FakeChat::new(vec![Ok(chat_response(
            r#"{"repo": "ghost", "confidence": 0.9, "reason": "x"}"#,
        ))]);
        let err = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ClassifyError::UnknownRepo(ref r) if r == "ghost"),
            "{err}"
        );

        let chat = FakeChat::new(vec![Err("connection refused".into())]);
        let err = classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap_err();
        assert!(matches!(err, ClassifyError::Request(_)), "{err}");
    }

    #[test]
    fn balanced_object_handles_nesting_and_strings() {
        assert_eq!(
            balanced_object(r#"{"a": {"b": "}"}} tail"#),
            Some(r#"{"a": {"b": "}"}}"#)
        );
        assert!(balanced_object("{unbalanced").is_none());
    }

    #[test]
    fn extraction_skips_prose_braces_before_the_verdict() {
        // A brace in prose before the verdict must not shadow it.
        let verdict = extract_verdict(
            r#"The {Button} component belongs there. {"repo": "web-app", "confidence": 0.9, "reason": "x"}"#,
        )
        .expect("the trailing verdict parses");
        assert_eq!(verdict.repo, "web-app");
        assert!(extract_verdict("no json here").is_none());
        assert!(extract_verdict("{unbalanced").is_none());
    }

    #[tokio::test]
    async fn retry_carries_a_correction_message() {
        let candidates = [repo("web-app")];
        let chat = FakeChat::new(vec![
            Ok(chat_response("just prose")),
            Ok(chat_response(
                r#"{"repo": "web-app", "confidence": 0.9, "reason": "ok"}"#,
            )),
        ]);
        classify(&chat, &config(), "m", "", &candidates)
            .await
            .unwrap();

        let requests = chat.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        // The retry is not a byte-identical resend: it echoes the malformed
        // answer and appends the corrective instruction.
        let retry_messages = requests[1]["messages"].as_array().unwrap();
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
}
