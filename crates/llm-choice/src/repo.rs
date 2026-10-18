//! The repository question: which of the configured repositories a piece of
//! work belongs to (F-11–F-14).
//!
//! Everything repository-specific lives here — the candidate shape, the fixed
//! wording, the built-in chat prompt — and is turned into a plain
//! [`ChoiceRequest`]. The bodies sent are the ones this crate sent before it
//! was generalised (ADR-0118), byte for byte.

use crate::{
    Answer, ChatPrompt, ChoiceClassifier, ChoiceOption, ChoiceRequest, ClassifyError, Question,
};

/// The question id: the decisions `questions` key, and the field a chat
/// verdict names its repository in (`{"repo": …}` — the shape every
/// `classifier_*` prompt override already asks for).
const QUESTION_ID: &str = "repo";

/// What a decisions model is asked. Fixed wording on purpose: the answer set
/// carries the meaning, and a configurable question would only be one more way
/// to make it disagree with its options.
const INSTRUCTIONS: &str = "Which repository should this work be done in? \
                            Choose `{none}` only when no repository fits.";

/// The description of the "nothing fits" option.
pub(crate) const NONE_FITS: &str = "None of the repositories fits this work";

/// A repository the work could target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Repository name — what a verdict must name exactly.
    pub name: String,
    /// Operator-written summary (F-61).
    pub summary: Option<String>,
    /// The first lines of the repository's README (F-11).
    pub readme_head: Option<String>,
}

/// One repository classification question.
#[derive(Debug, Clone)]
pub struct ClassifyRequest {
    /// What is being classified, as labelled fields in display order.
    pub subject: Vec<(String, String)>,
    /// The repositories to choose from.
    pub candidates: Vec<Candidate>,
    /// A caller-rendered chat prompt (the Slack plugin's configurable
    /// templates). `None` uses [`default_prompt`].
    pub chat_prompt: Option<ChatPrompt>,
}

/// A validated repository verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum Classification {
    /// One of the candidates.
    Repo {
        /// The chosen candidate's name.
        repo: String,
        /// How sure the backend is, `0.0..=1.0`.
        confidence: f64,
        /// Why — surfaced by `--dry-run` and the logs, never decided on.
        reason: String,
    },
    /// The backend judged that none of the candidates fits (decisions only).
    NoneFits {
        /// Why.
        reason: String,
    },
}

impl From<Answer> for Classification {
    fn from(answer: Answer) -> Self {
        match answer {
            Answer::Chosen {
                key,
                confidence,
                reason,
            } => Self::Repo {
                repo: key,
                confidence,
                reason,
            },
            Answer::NoneFits { reason } => Self::NoneFits { reason },
        }
    }
}

impl From<Classification> for Answer {
    fn from(verdict: Classification) -> Self {
        match verdict {
            Classification::Repo {
                repo,
                confidence,
                reason,
            } => Self::Chosen {
                key: repo,
                confidence,
                reason,
            },
            Classification::NoneFits { reason } => Self::NoneFits { reason },
        }
    }
}

/// Ask `classifier` which repository `request` belongs to.
pub async fn classify<C: ChoiceClassifier>(
    classifier: &C,
    request: &ClassifyRequest,
) -> Result<Classification, ClassifyError> {
    classifier
        .choose(&choice_request(request))
        .await
        .map(Classification::from)
}

/// The repository question as a [`ChoiceRequest`].
pub fn choice_request(request: &ClassifyRequest) -> ChoiceRequest {
    ChoiceRequest {
        subject: request.subject.clone(),
        question: question(&request.candidates),
        chat_prompt: request
            .chat_prompt
            .clone()
            .unwrap_or_else(|| default_prompt(&request.subject, &request.candidates)),
    }
}

/// One option per candidate, keyed by its name (verified to accept `/`, `.`
/// and `_` as decisions keys), described by its summary and README head;
/// plus the "nothing fits" option.
pub fn question(candidates: &[Candidate]) -> Question {
    Question {
        id: QUESTION_ID.to_string(),
        instructions: INSTRUCTIONS.to_string(),
        options: candidates
            .iter()
            .map(|c| ChoiceOption {
                key: c.name.clone(),
                description: description(c),
            })
            .collect(),
        none: Some(NONE_FITS.to_string()),
    }
}

/// A candidate's option description; `None` when there is nothing to say.
fn description(c: &Candidate) -> Option<String> {
    let parts: Vec<String> = [
        c.summary.clone(),
        c.readme_head
            .as_ref()
            .map(|r| format!("README (head):\n{r}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The built-in chat prompt: the labelled subject, then each candidate with
/// its summary and README head.
pub fn default_prompt(subject: &[(String, String)], candidates: &[Candidate]) -> ChatPrompt {
    let mut user = String::new();
    for (label, text) in subject {
        user.push_str(&format!("{label}: {text}\n"));
    }
    user.push_str("\nCandidate repositories:\n");
    for c in candidates {
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
