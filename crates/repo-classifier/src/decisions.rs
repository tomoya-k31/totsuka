//! [`DecisionsClassifier`]: classification by a decisions model (TypeSafe Jev)
//! — one typed `choice` question instead of a chat.
//!
//! A decisions model does not generate text. It is sent the work (`state`) and
//! a question whose answer set is fixed (`criteria`), and returns one of those
//! answers with a probability distribution over all of them. An answer outside
//! the set is impossible by construction, so there is no prompt engineering,
//! no JSON extraction and no "unreadable answer" retry to speak of.
//!
//! The wire format is OpenRouter's Decisions API, `POST /api/alpha/decisions`
//! (TypeSafe's own `POST /v1/systemone` takes the same body). The endpoint is
//! **alpha** on OpenRouter's side, so it is configurable rather than assumed.

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::{
    ApiKey, Candidate, Classification, ClassifyError, ClassifyRequest, HttpRequest, HttpTransport,
    RepoClassifier, RetryPolicy, validated,
};

/// The question id in `questions` / `answers`.
const QUESTION: &str = "repo";

/// What the model is asked. Fixed on purpose: the answer set carries the
/// meaning, and a configurable wording would only be one more way to make the
/// question disagree with its options.
const INSTRUCTIONS: &str = "Which repository should this work be done in? \
                            Choose `none` only when no repository fits.";

/// The description of the "nothing fits" option.
const NONE_FITS: &str = "None of the repositories fits this work";

/// Where and how to call the decisions endpoint.
#[derive(Debug, Clone)]
pub struct DecisionsSettings {
    /// The full endpoint URL — not a base URL: gateways put the Decisions API
    /// at different paths.
    pub endpoint: String,
    /// Model identifier (`~typesafe/jev-latest`, `typesafe/jev-1.13`).
    pub model: String,
    /// Sent as the bearer token.
    pub api_key: ApiKey,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Retries for transport failures.
    pub retry: RetryPolicy,
}

impl DecisionsSettings {
    /// OpenRouter's Decisions API.
    pub const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

    /// The Orchestrator's defaults: 30s timeout, [`RetryPolicy::STANDARD`].
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>, api_key: ApiKey) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key,
            timeout: Duration::from_secs(30),
            retry: RetryPolicy::STANDARD,
        }
    }
}

/// A [`RepoClassifier`] over a Decisions API.
#[derive(Debug)]
pub struct DecisionsClassifier<T> {
    transport: T,
    settings: DecisionsSettings,
}

impl<T: HttpTransport> DecisionsClassifier<T> {
    /// A classifier sending through `transport`.
    pub fn new(transport: T, settings: DecisionsSettings) -> Self {
        Self {
            transport,
            settings,
        }
    }

    /// The configured settings.
    pub fn settings(&self) -> &DecisionsSettings {
        &self.settings
    }

    fn request<'a>(&'a self, body: &'a Value) -> HttpRequest<'a> {
        HttpRequest {
            url: &self.settings.endpoint,
            api_key: &self.settings.api_key,
            body,
            timeout: self.settings.timeout,
        }
    }
}

impl<T: HttpTransport> RepoClassifier for DecisionsClassifier<T> {
    async fn classify(&self, request: &ClassifyRequest) -> Result<Classification, ClassifyError> {
        let none = none_key(&request.candidates);
        let body = json!({
            "model": self.settings.model,
            "state": state(&request.subject),
            "questions": {
                QUESTION: {
                    "type": "choice",
                    "instructions": INSTRUCTIONS,
                    "criteria": criteria(&request.candidates, &none),
                },
            },
        });
        let response = self
            .settings
            .retry
            .run(|| self.transport.post_json(self.request(&body)))
            .await?;
        verdict(&response, &none, &request.candidates)
    }

    /// The cheapest question the endpoint answers: one yes/no about a
    /// one-word state. Measured at ~270 input tokens (~$0.00001) on
    /// OpenRouter; the answer is discarded, a 2xx is the whole answer.
    async fn probe(&self) -> Result<(), ClassifyError> {
        let body = json!({
            "model": self.settings.model,
            "state": "ping",
            "questions": { "alive": { "type": "noul", "instructions": "Is this a ping?" } },
        });
        match self.transport.post_json(self.request(&body)).await {
            Ok(_) | Err(ClassifyError::InvalidResponse(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn reset_connections(&self) {
        self.transport.reset_connections();
    }
}

/// The subject as a named-field object — the shape TypeSafe recommends for
/// state, so each part keeps its label.
fn state(subject: &[(String, String)]) -> Value {
    Value::Object(
        subject
            .iter()
            .map(|(label, text)| (label.clone(), Value::String(text.clone())))
            .collect(),
    )
}

/// One option per candidate, keyed by its name (verified to accept `/`, `.`
/// and `_`), described by its summary and README head; plus the `none`
/// option.
fn criteria(candidates: &[Candidate], none: &str) -> Value {
    let mut options: Map<String, Value> = candidates
        .iter()
        .map(|c| (c.name.clone(), description(c)))
        .collect();
    options.insert(none.to_string(), Value::String(NONE_FITS.to_string()));
    Value::Object(options)
}

/// A candidate's option description; `null` (allowed by the API: "no
/// description") when there is nothing to say.
fn description(c: &Candidate) -> Value {
    let parts: Vec<String> = [
        c.summary.clone(),
        c.readme_head
            .as_ref()
            .map(|r| format!("README (head):\n{r}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        Value::Null
    } else {
        Value::String(parts.join("\n\n"))
    }
}

/// The key of the "nothing fits" option: `none`, unless a repository is
/// literally called that, in which case underscores are prepended until it is
/// unique.
fn none_key(candidates: &[Candidate]) -> String {
    let mut key = "none".to_string();
    while candidates.iter().any(|c| c.name == key) {
        key.insert(0, '_');
    }
    key
}

/// Read the answer to [`QUESTION`].
///
/// The value compared against thresholds is the chosen option's own
/// probability, which reads like a chat model's self-reported confidence
/// ("84% it is totsuka"). The API's separate `confidence` is how concentrated
/// the whole distribution is — a different statistic (0.6 for that same 84%)
/// — and is only the fallback when `probabilities` is absent, which
/// OpenRouter's schema allows.
fn verdict(
    response: &Value,
    none: &str,
    candidates: &[Candidate],
) -> Result<Classification, ClassifyError> {
    let invalid = |why: String| ClassifyError::InvalidResponse(why);
    let answer = &response["answers"][QUESTION];
    let choice = answer["choice"]
        .as_str()
        .ok_or_else(|| invalid(format!("no `answers.{QUESTION}.choice` in the response")))?;
    let probabilities = answer["probabilities"].as_object();
    let reason = explain(response["model"].as_str(), choice, probabilities);
    if choice == none {
        return Ok(Classification::NoneFits { reason });
    }
    let confidence = probabilities
        .and_then(|p| p.get(choice))
        .and_then(Value::as_f64)
        .or_else(|| answer["confidence"].as_f64())
        .ok_or_else(|| {
            invalid(format!(
                "neither a probability nor a confidence for `{choice}`"
            ))
        })?;
    validated(choice.to_string(), confidence, reason, candidates)
}

/// A decisions model gives no rationale, so the reason is the distribution:
/// `typesafe/jev-1.13: totsuka p=0.84 (next: dotfiles 0.15)`.
fn explain(
    model: Option<&str>,
    choice: &str,
    probabilities: Option<&Map<String, Value>>,
) -> String {
    let mut out = format!("{}: {choice}", model.unwrap_or("decisions"));
    let Some(probabilities) = probabilities else {
        return out;
    };
    if let Some(p) = probabilities.get(choice).and_then(Value::as_f64) {
        out.push_str(&format!(" p={p:.2}"));
    }
    let runner_up = probabilities
        .iter()
        .filter(|(name, _)| name.as_str() != choice)
        .filter_map(|(name, p)| p.as_f64().map(|p| (name, p)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((name, p)) = runner_up {
        out.push_str(&format!(" (next: {name} {p:.2})"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Canned {
        answers: Mutex<VecDeque<Result<Value, ClassifyError>>>,
        requests: Mutex<Vec<(String, Value)>>,
    }

    impl HttpTransport for Canned {
        async fn post_json(&self, request: HttpRequest<'_>) -> Result<Value, ClassifyError> {
            self.requests
                .lock()
                .unwrap()
                .push((request.url.to_string(), request.body.clone()));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ClassifyError::Transport("no canned answer".into())))
        }
    }

    fn classifier(answers: Vec<Result<Value, ClassifyError>>) -> DecisionsClassifier<Canned> {
        let mut settings = DecisionsSettings::new(
            DecisionsSettings::OPENROUTER_ENDPOINT,
            "~typesafe/jev-latest",
            ApiKey::new("k"),
        );
        settings.retry = RetryPolicy::NONE;
        DecisionsClassifier::new(
            Canned {
                answers: Mutex::new(answers.into()),
                ..Canned::default()
            },
            settings,
        )
    }

    fn candidate(name: &str, summary: Option<&str>, readme: Option<&str>) -> Candidate {
        Candidate {
            name: name.into(),
            summary: summary.map(str::to_string),
            readme_head: readme.map(str::to_string),
        }
    }

    fn request() -> ClassifyRequest {
        ClassifyRequest {
            subject: vec![
                ("Task".into(), "Fix the Slack trigger".into()),
                ("Description".into(), "ignore bot posts".into()),
            ],
            candidates: vec![
                candidate(
                    "tomoya-k31/totsuka",
                    Some("Rust orchestrator"),
                    Some("# totsuka"),
                ),
                candidate("dotfiles.nvim", Some("dotfiles"), None),
                candidate("bare", None, None),
            ],
            chat_prompt: None,
        }
    }

    /// The shape of a real answer, captured from OpenRouter on 2026-09-19.
    fn answer(choice: &str, probabilities: Value, confidence: f64) -> Result<Value, ClassifyError> {
        Ok(json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": { "repo": {
                "type": "choice",
                "choice": choice,
                "probabilities": probabilities,
                "confidence": confidence,
            } },
            "usage": { "input_tokens": 440, "output_tokens": 67, "cost": 0.00001848 },
            "id": "gen-dec-1",
            "provider": "TypeSafe",
        }))
    }

    #[tokio::test]
    async fn the_request_is_one_choice_over_every_candidate_plus_none() {
        let c = classifier(vec![answer(
            "tomoya-k31/totsuka",
            json!({ "tomoya-k31/totsuka": 1, "dotfiles.nvim": 0, "bare": 0, "none": 0 }),
            1.0,
        )]);
        c.classify(&request()).await.unwrap();

        let requests = c.transport.requests.lock().unwrap();
        let (url, body) = &requests[0];
        assert_eq!(url, "https://openrouter.ai/api/alpha/decisions");
        assert_eq!(body["model"], "~typesafe/jev-latest");
        assert_eq!(
            body["state"],
            json!({ "Task": "Fix the Slack trigger", "Description": "ignore bot posts" })
        );
        let question = &body["questions"]["repo"];
        assert_eq!(question["type"], "choice");
        assert_eq!(
            question["criteria"],
            json!({
                "tomoya-k31/totsuka": "Rust orchestrator\n\nREADME (head):\n# totsuka",
                "dotfiles.nvim": "dotfiles",
                "bare": null,
                "none": NONE_FITS,
            })
        );
    }

    #[tokio::test]
    async fn the_chosen_options_probability_is_the_confidence() {
        let c = classifier(vec![answer(
            "tomoya-k31/totsuka",
            json!({ "tomoya-k31/totsuka": 0.84, "dotfiles.nvim": 0.15, "bare": 0.0, "none": 0.01 }),
            0.6,
        )]);
        assert_eq!(
            c.classify(&request()).await.unwrap(),
            Classification::Repo {
                repo: "tomoya-k31/totsuka".into(),
                confidence: 0.84,
                reason: "typesafe/jev-1.13-20260917: tomoya-k31/totsuka p=0.84 (next: dotfiles.nvim 0.15)"
                    .into(),
            }
        );
    }

    #[tokio::test]
    async fn without_probabilities_the_confidence_field_is_the_fallback() {
        let c = classifier(vec![Ok(json!({
            "model": "m",
            "answers": { "repo": { "type": "choice", "choice": "bare", "confidence": 0.7 } },
        }))]);
        let verdict = c.classify(&request()).await.unwrap();
        assert!(
            matches!(verdict, Classification::Repo { confidence, .. } if (confidence - 0.7).abs() < 1e-9),
            "{verdict:?}"
        );
    }

    #[tokio::test]
    async fn neither_probability_nor_confidence_is_a_bad_answer() {
        let c = classifier(vec![Ok(json!({
            "answers": { "repo": { "type": "choice", "choice": "bare" } },
        }))]);
        assert!(c.classify(&request()).await.unwrap_err().is_bad_answer());
    }

    #[tokio::test]
    async fn choosing_none_means_none_fits() {
        let c = classifier(vec![answer(
            "none",
            json!({ "tomoya-k31/totsuka": 0.02, "dotfiles.nvim": 0, "bare": 0, "none": 0.98 }),
            0.97,
        )]);
        assert_eq!(
            c.classify(&request()).await.unwrap(),
            Classification::NoneFits {
                reason: "typesafe/jev-1.13-20260917: none p=0.98 (next: tomoya-k31/totsuka 0.02)"
                    .into()
            }
        );
    }

    #[tokio::test]
    async fn an_answer_outside_the_candidates_is_rejected() {
        let c = classifier(vec![answer("ghost", json!({ "ghost": 1 }), 1.0)]);
        assert!(matches!(
            c.classify(&request()).await.unwrap_err(),
            ClassifyError::UnknownRepo(r) if r == "ghost"
        ));
        let c = classifier(vec![Ok(json!({ "answers": {} }))]);
        assert!(c.classify(&request()).await.unwrap_err().is_bad_answer());
    }

    #[test]
    fn the_none_key_never_collides_with_a_repository() {
        assert_eq!(none_key(&[candidate("web", None, None)]), "none");
        assert_eq!(
            none_key(&[
                candidate("none", None, None),
                candidate("_none", None, None)
            ]),
            "__none"
        );
    }

    #[tokio::test]
    async fn the_probe_is_one_unretried_yes_no_question() {
        let c = classifier(vec![Ok(
            json!({ "answers": { "alive": { "noul": 0.69 } } }),
        )]);
        c.probe().await.unwrap();
        let body = c.transport.requests.lock().unwrap()[0].1.clone();
        assert_eq!(body["questions"]["alive"]["type"], "noul");

        let c = classifier(vec![Err(ClassifyError::status(401, ""))]);
        assert!(c.probe().await.unwrap_err().is_auth_failure());
        assert_eq!(c.transport.requests.lock().unwrap().len(), 1);
    }
}
