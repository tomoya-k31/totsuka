//! [`ChatClassifier`]: classification over an OpenAI-compatible
//! `/chat/completions` endpoint (F-12, F-13).

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiKey, Candidate, Classification, ClassifyError, ClassifyRequest, HttpRequest, HttpTransport,
    RepoClassifier, RetryPolicy, validated,
};

/// A fully rendered chat prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatPrompt {
    /// The system message (the output contract).
    pub system: String,
    /// The user message (the work + the candidates).
    pub user: String,
    /// When set, an unreadable answer is retried **once** inside
    /// [`classify`](RepoClassifier::classify): the malformed answer is echoed
    /// back as the assistant turn and this text appended as a user turn.
    /// Resending an identical body at temperature 0 would only repeat the
    /// failure. `None` returns the unreadable answer as
    /// [`ClassifyError::InvalidResponse`] and leaves retrying to the caller.
    pub correction: Option<String>,
}

/// How the model is asked to shape its answer, and so how strictly it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatOutput {
    /// `response_format: json_schema` (strict): the content must be exactly
    /// the verdict object, `reason` included. For gateways and models that
    /// support structured output.
    JsonSchema,
    /// No `response_format`; the prompt asks for JSON and the verdict is
    /// extracted from whatever prose or code fences surround it. `reason` is
    /// optional. For models that reject `json_schema`.
    Prose,
}

/// Where and how to call the chat endpoint.
#[derive(Debug, Clone)]
pub struct ChatSettings {
    /// OpenAI-compatible base URL (`…/v1`); `/chat/completions` is appended.
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// Sent as the bearer token.
    pub api_key: ApiKey,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Retries for transport failures (never for a bad answer).
    pub retry: RetryPolicy,
    /// `max_tokens`, sent only when set: OpenAI-compatible APIs often reject a
    /// literal `null` for numeric fields with a 400.
    pub max_tokens: Option<u32>,
    /// `temperature`, sent only when set.
    pub temperature: Option<f64>,
    /// The answer contract.
    pub output: ChatOutput,
}

impl ChatSettings {
    /// The Orchestrator's defaults: 30s timeout, [`RetryPolicy::STANDARD`],
    /// structured output, provider-default sampling.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: ApiKey) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            timeout: Duration::from_secs(30),
            retry: RetryPolicy::STANDARD,
            max_tokens: None,
            temperature: None,
            output: ChatOutput::JsonSchema,
        }
    }
}

/// A [`RepoClassifier`] over `/chat/completions`.
#[derive(Debug)]
pub struct ChatClassifier<T> {
    transport: T,
    settings: ChatSettings,
}

impl<T: HttpTransport> ChatClassifier<T> {
    /// A classifier sending through `transport`.
    pub fn new(transport: T, settings: ChatSettings) -> Self {
        Self {
            transport,
            settings,
        }
    }

    /// The configured settings.
    pub fn settings(&self) -> &ChatSettings {
        &self.settings
    }

    /// The `/chat/completions` URL.
    pub fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.settings.base_url.trim_end_matches('/')
        )
    }

    /// One POST, with transport retries.
    async fn complete(&self, body: &Value) -> Result<Value, ClassifyError> {
        let url = self.endpoint();
        self.settings
            .retry
            .run(|| {
                self.transport.post_json(HttpRequest {
                    url: &url,
                    api_key: &self.settings.api_key,
                    body,
                    timeout: self.settings.timeout,
                })
            })
            .await
    }

    /// The request body for `prompt`, constraining the answer to `candidates`
    /// when the output mode asks for a schema.
    fn body(&self, prompt: &ChatPrompt, candidates: &[Candidate]) -> Value {
        let mut body = json!({
            "model": self.settings.model,
            "messages": [
                { "role": "system", "content": prompt.system },
                { "role": "user", "content": prompt.user },
            ],
        });
        if self.settings.output == ChatOutput::JsonSchema {
            let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "structured_output",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "repo": { "type": "string", "enum": names },
                            "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                            "reason": { "type": "string" },
                        },
                        "required": ["repo", "confidence", "reason"],
                        "additionalProperties": false,
                    },
                    "strict": true,
                },
            });
        }
        if let Some(max_tokens) = self.settings.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = self.settings.temperature {
            body["temperature"] = json!(temperature);
        }
        body
    }

    /// Read the verdict out of a chat-completions response.
    ///
    /// The error side carries the raw content, so a correction retry can echo
    /// it back.
    fn verdict(&self, response: &Value) -> Result<RawVerdict, (String, ClassifyError)> {
        let Some(content) = response["choices"][0]["message"]["content"].as_str() else {
            return Err((
                String::new(),
                ClassifyError::InvalidResponse("missing choices[0].message.content".into()),
            ));
        };
        let parsed = match self.settings.output {
            ChatOutput::JsonSchema => strict_verdict(content),
            ChatOutput::Prose => extract_verdict(content).ok_or_else(|| {
                ClassifyError::InvalidResponse(format!(
                    "no JSON verdict in content: {content:.200}"
                ))
            }),
        };
        parsed.map_err(|e| (content.to_string(), e))
    }
}

impl<T: HttpTransport> RepoClassifier for ChatClassifier<T> {
    async fn classify(&self, request: &ClassifyRequest) -> Result<Classification, ClassifyError> {
        let prompt = request
            .chat_prompt
            .clone()
            .unwrap_or_else(|| default_prompt(request));
        let mut body = self.body(&prompt, &request.candidates);

        let response = self.complete(&body).await?;
        let raw = match (self.verdict(&response), &prompt.correction) {
            (Ok(raw), _) => raw,
            (Err((_, e)), None) => return Err(e),
            (Err((content, e)), Some(correction)) => {
                tracing::info!(error = %e, "classifier verdict malformed; retrying with a correction");
                if let Some(messages) = body["messages"].as_array_mut() {
                    messages.push(json!({ "role": "assistant", "content": content }));
                    messages.push(json!({ "role": "user", "content": correction }));
                }
                let response = self.complete(&body).await?;
                self.verdict(&response).map_err(|(_, e)| e)?
            }
        };
        validated(raw.repo, raw.confidence, raw.reason, &request.candidates)
    }

    /// One minimal request asking only whether the gateway accepts our key
    /// (`doctor --online` #267, and the engine's liveness probe F-111).
    ///
    /// No schema: providers differ in what structured-output shapes they
    /// accept, and a rejected schema (400) would masquerade as a credentials
    /// problem. No retries. `max_tokens: 1`, so a healthy provider bills a
    /// rounding error. A 2xx is the whole answer: a body that is unreadable or
    /// not JSON still means the key was accepted.
    async fn probe(&self) -> Result<(), ClassifyError> {
        let body = json!({
            "model": self.settings.model,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
        });
        let outcome = self
            .transport
            .post_json(HttpRequest {
                url: &self.endpoint(),
                api_key: &self.settings.api_key,
                body: &body,
                timeout: self.settings.timeout,
            })
            .await;
        match outcome {
            // `InvalidResponse` only ever follows a 2xx: the key was accepted.
            Ok(_) | Err(ClassifyError::InvalidResponse(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn reset_connections(&self) {
        self.transport.reset_connections();
    }
}

/// The built-in prompt: the labelled subject, then each candidate with its
/// summary and README head.
fn default_prompt(request: &ClassifyRequest) -> ChatPrompt {
    let mut user = String::new();
    for (label, text) in &request.subject {
        user.push_str(&format!("{label}: {text}\n"));
    }
    user.push_str("\nCandidate repositories:\n");
    for c in &request.candidates {
        user.push_str(&format!("- {}", c.name));
        if let Some(s) = &c.summary {
            user.push_str(&format!(": {s}"));
        }
        if let Some(r) = &c.readme_head {
            user.push_str(&format!("\n  README: {}", r.replace('\n', " ")));
        }
        user.push('\n');
    }
    user.push_str("\nChoose the single most appropriate repository.");
    ChatPrompt {
        system: "You route a software task to the correct repository. Respond only with the \
                 structured JSON. Pick from the candidate names exactly."
            .to_string(),
        user,
        correction: None,
    }
}

/// A verdict as the model wrote it, before candidate/range validation.
#[derive(Debug, Deserialize)]
struct RawVerdict {
    repo: String,
    confidence: f64,
    #[serde(default)]
    reason: String,
}

/// [`ChatOutput::JsonSchema`]: the content is the verdict object, nothing
/// else, and every schema field is present. A missing `reason` is a schema
/// deviation, not an empty rationale.
fn strict_verdict(content: &str) -> Result<RawVerdict, ClassifyError> {
    let invalid = |why: &str| ClassifyError::InvalidResponse(why.to_string());
    let value: Value =
        serde_json::from_str(content).map_err(|e| ClassifyError::InvalidResponse(e.to_string()))?;
    Ok(RawVerdict {
        repo: value["repo"]
            .as_str()
            .ok_or_else(|| invalid("missing `repo`"))?
            .to_string(),
        confidence: value["confidence"]
            .as_f64()
            .ok_or_else(|| invalid("missing/invalid `confidence`"))?,
        reason: value["reason"]
            .as_str()
            .ok_or_else(|| invalid("missing/invalid `reason`"))?
            .to_string(),
    })
}

/// [`ChatOutput::Prose`]: try every `{` in `text` as the start of a balanced
/// JSON object and return the first that reads as a verdict. Anchoring on the
/// *first* brace only would let prose like `the {Button} component` shadow a
/// valid verdict later in the answer.
fn extract_verdict(text: &str) -> Option<RawVerdict> {
    text.char_indices()
        .filter(|(_, c)| *c == '{')
        .filter_map(|(start, _)| balanced_object(&text[start..]))
        .find_map(|candidate| serde_json::from_str(candidate).ok())
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
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Answers from a canned queue and records every body it was sent.
    #[derive(Default)]
    struct Canned {
        answers: Mutex<VecDeque<Result<Value, ClassifyError>>>,
        bodies: Mutex<Vec<Value>>,
    }

    impl Canned {
        fn new(answers: Vec<Result<Value, ClassifyError>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                ..Self::default()
            }
        }
    }

    impl HttpTransport for Canned {
        async fn post_json(&self, request: HttpRequest<'_>) -> Result<Value, ClassifyError> {
            self.bodies.lock().unwrap().push(request.body.clone());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ClassifyError::Transport("no canned answer".into())))
        }
    }

    /// `(repo, reason)` of a verdict that must name a candidate.
    fn chosen(verdict: &Classification) -> (&str, &str) {
        match verdict {
            Classification::Repo { repo, reason, .. } => (repo, reason),
            other => panic!("a chat verdict always names a candidate: {other:?}"),
        }
    }

    fn content(text: &str) -> Result<Value, ClassifyError> {
        Ok(json!({ "choices": [{ "message": { "content": text } }] }))
    }

    fn request(chat_prompt: Option<ChatPrompt>) -> ClassifyRequest {
        ClassifyRequest {
            subject: vec![
                ("Task".into(), "Fix the login bug".into()),
                ("Description".into(), "Users cannot sign in".into()),
            ],
            candidates: vec![
                Candidate {
                    name: "web".into(),
                    summary: Some("frontend".into()),
                    readme_head: Some("# Web\nThe site".into()),
                },
                Candidate {
                    name: "api".into(),
                    summary: Some("backend".into()),
                    readme_head: None,
                },
            ],
            chat_prompt,
        }
    }

    fn classifier(
        output: ChatOutput,
        answers: Vec<Result<Value, ClassifyError>>,
    ) -> ChatClassifier<Canned> {
        let mut settings = ChatSettings::new("https://gw.test/v1/", "m", ApiKey::new("k"));
        settings.output = output;
        settings.retry = RetryPolicy::NONE;
        ChatClassifier::new(Canned::new(answers), settings)
    }

    fn prose_prompt() -> ChatPrompt {
        ChatPrompt {
            system: "sys".into(),
            user: "usr".into(),
            correction: Some("answer with only the JSON".into()),
        }
    }

    #[tokio::test]
    async fn the_built_in_prompt_and_schema_describe_every_candidate() {
        let c = classifier(
            ChatOutput::JsonSchema,
            vec![content(
                r#"{"repo":"api","confidence":0.9,"reason":"backend"}"#,
            )],
        );
        let verdict = c.classify(&request(None)).await.unwrap();
        assert_eq!(chosen(&verdict), ("api", "backend"));

        let bodies = c.transport.bodies.lock().unwrap();
        let body = &bodies[0];
        assert_eq!(c.endpoint(), "https://gw.test/v1/chat/completions");
        assert_eq!(
            body["messages"][1]["content"],
            "Task: Fix the login bug\nDescription: Users cannot sign in\n\nCandidate repositories:\n\
             - web: frontend\n  README: # Web The site\n- api: backend\n\n\
             Choose the single most appropriate repository."
        );
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["properties"]["repo"]["enum"],
            json!(["web", "api"])
        );
        assert!(body.get("max_tokens").is_none(), "unset → not sent");
        assert!(body.get("temperature").is_none(), "unset → not sent");
    }

    #[tokio::test]
    async fn json_schema_mode_rejects_every_deviation_without_retrying() {
        for (answer, needle) in [
            (r#"{"repo":"api","confidence":0.9}"#, "reason"),
            (
                r#"{"repo":"api","confidence":1.5,"reason":"x"}"#,
                "out of range",
            ),
            (r#"{"confidence":0.9,"reason":"x"}"#, "repo"),
            ("I think api", "expected"),
        ] {
            let c = classifier(ChatOutput::JsonSchema, vec![content(answer)]);
            let err = c.classify(&request(None)).await.unwrap_err();
            assert!(err.is_bad_answer(), "{answer}: {err}");
            assert!(err.to_string().contains(needle), "{answer}: {err}");
            assert_eq!(c.transport.bodies.lock().unwrap().len(), 1, "no retry");
        }
    }

    #[tokio::test]
    async fn an_unknown_repository_is_its_own_error() {
        let c = classifier(
            ChatOutput::JsonSchema,
            vec![content(r#"{"repo":"ghost","confidence":0.9,"reason":"x"}"#)],
        );
        assert!(matches!(
            c.classify(&request(None)).await.unwrap_err(),
            ClassifyError::UnknownRepo(r) if r == "ghost"
        ));
    }

    #[tokio::test]
    async fn prose_mode_tolerates_fences_and_a_missing_reason() {
        let c = classifier(
            ChatOutput::Prose,
            vec![content(
                "Sure! ```json\n{\"repo\": \"web\", \"confidence\": 0.8}\n```",
            )],
        );
        let verdict = c.classify(&request(Some(prose_prompt()))).await.unwrap();
        assert_eq!(chosen(&verdict), ("web", ""));
        let body = &c.transport.bodies.lock().unwrap()[0];
        assert!(body.get("response_format").is_none());
        assert_eq!(body["messages"][0]["content"], "sys");
        assert_eq!(body["messages"][1]["content"], "usr");
    }

    #[tokio::test]
    async fn a_correction_prompt_retries_once_with_the_bad_answer_echoed() {
        let c = classifier(
            ChatOutput::Prose,
            vec![
                content("just prose"),
                content(r#"{"repo":"api","confidence":0.7,"reason":"r"}"#),
            ],
        );
        let verdict = c.classify(&request(Some(prose_prompt()))).await.unwrap();
        assert_eq!(chosen(&verdict).0, "api");

        let bodies = c.transport.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let retry = bodies[1]["messages"].as_array().unwrap();
        assert_eq!(retry.len(), 4, "{retry:?}");
        assert_eq!(
            retry[2],
            json!({ "role": "assistant", "content": "just prose" })
        );
        assert_eq!(retry[3]["content"], "answer with only the JSON");
    }

    #[tokio::test]
    async fn a_second_unreadable_answer_is_invalid() {
        let c = classifier(
            ChatOutput::Prose,
            vec![content("nope"), content("still nope")],
        );
        let err = c
            .classify(&request(Some(prose_prompt())))
            .await
            .unwrap_err();
        assert!(matches!(err, ClassifyError::InvalidResponse(_)), "{err}");
        assert_eq!(
            c.transport.bodies.lock().unwrap().len(),
            2,
            "exactly one retry"
        );
    }

    /// The correction retry is for unreadable answers only: a readable verdict
    /// naming a non-candidate is final.
    #[tokio::test]
    async fn an_unknown_repository_is_not_corrected() {
        let c = classifier(
            ChatOutput::Prose,
            vec![content(r#"{"repo":"ghost","confidence":0.9}"#)],
        );
        let err = c
            .classify(&request(Some(prose_prompt())))
            .await
            .unwrap_err();
        assert!(matches!(err, ClassifyError::UnknownRepo(_)), "{err}");
        assert_eq!(c.transport.bodies.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sampling_settings_are_sent_when_set() {
        let mut c = classifier(
            ChatOutput::JsonSchema,
            vec![content(r#"{"repo":"api","confidence":0.9,"reason":"r"}"#)],
        );
        c.settings.max_tokens = Some(256);
        c.settings.temperature = Some(0.0);
        c.classify(&request(None)).await.unwrap();
        let body = &c.transport.bodies.lock().unwrap()[0];
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["temperature"], 0.0);
    }

    #[tokio::test]
    async fn the_probe_sends_no_schema_and_ignores_the_body() {
        let c = classifier(
            ChatOutput::JsonSchema,
            vec![Ok(json!("not a chat response"))],
        );
        c.probe().await.unwrap();
        let body = c.transport.bodies.lock().unwrap()[0].clone();
        assert!(body.get("response_format").is_none());
        assert_eq!(body["max_tokens"], 1);

        let c = classifier(
            ChatOutput::JsonSchema,
            vec![Err(ClassifyError::status(401, ""))],
        );
        assert!(c.probe().await.unwrap_err().is_auth_failure());
    }

    #[test]
    fn balanced_object_handles_nesting_and_strings() {
        assert_eq!(
            balanced_object(r#"{"a":{"b":"}"}} tail"#),
            Some(r#"{"a":{"b":"}"}}"#)
        );
        assert!(balanced_object("{unbalanced").is_none());
    }

    #[test]
    fn extraction_skips_prose_braces_before_the_verdict() {
        let verdict =
            extract_verdict(r#"the {Button} component → {"repo":"web","confidence":0.9}"#).unwrap();
        assert_eq!(verdict.repo, "web");
        assert!(extract_verdict("no json here").is_none());
    }
}
