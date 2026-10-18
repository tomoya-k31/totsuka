//! Asking an LLM to choose one option out of a fixed set (ADR-0118).
//!
//! The first question was *which configured repository a piece of work
//! belongs to* (F-11–F-14), asked by the Orchestrator (`orchestrator-core`'s
//! `repo_select`, for tasks that arrive without a usable repo hint) and by
//! task_source plugins that resolve the repository themselves before
//! submitting (`task-source-slack`). That question now lives in [`repo`]; the
//! crate itself only knows "here is the subject, here are the options, pick
//! one", so a second question (does this Slack mention need a reply?) rides
//! the same two backends instead of growing a copy of them.
//!
//! The boundary is the *question*, not the wire format: callers hand a
//! [`ChoiceRequest`] (what is being judged + the [`Question`]) to a
//! [`ChoiceClassifier`] and get a validated [`Answer`] back. Policy stays with
//! the caller — the confidence threshold, what to do on a low-confidence or
//! unusable answer, and whether to retry a bad answer at all.
//!
//! Layers:
//!
//! - [`HttpTransport`] — one authenticated JSON POST. The seam tests fake;
//!   [`ReqwestTransport`] is production.
//! - [`RetryPolicy`] — exponential backoff over retryable failures (§5.3).
//! - [`ChatClassifier`] — an OpenAI-compatible `/chat/completions` backend.
//! - [`DecisionsClassifier`] — a decisions-model backend (TypeSafe Jev via
//!   OpenRouter's Decisions API): a typed `choice` question instead of a chat.
//! - [`ConfiguredClassifier`] — whichever of the two the configuration asked
//!   for, behind one type.
//! - [`repo`] — the repository question built on top.

mod chat;
mod configured;
mod decisions;
mod error;
pub mod repo;
mod retry;
mod transport;

use std::future::Future;

pub use chat::{ChatClassifier, ChatOutput, ChatPrompt, ChatSettings};
pub use configured::ConfiguredClassifier;
pub use decisions::{DecisionsClassifier, DecisionsSettings};
pub use error::ClassifyError;
pub use retry::RetryPolicy;
pub use transport::{ApiKey, HttpRequest, HttpTransport, ReqwestTransport, scrub_urls};

/// One answer the backend may give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// What an answer must name exactly.
    pub key: String,
    /// What the option means. `None` sends no description.
    pub description: Option<String>,
}

/// What is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The question's id: the key in a decisions request's `questions` /
    /// `answers`, and the field a chat answer names its choice in
    /// (`{"<id>": …, "confidence": …, "reason": …}`).
    pub id: String,
    /// The question as a decisions model reads it. A literal `{none}` is
    /// replaced with the actual key of the "nothing fits" option (see
    /// [`none`](Self::none)). Chat backends ignore it: their prompt is
    /// [`ChoiceRequest::chat_prompt`].
    pub instructions: String,
    /// The answers to choose from. An answer outside this list is rejected as
    /// [`ClassifyError::UnknownChoice`].
    pub options: Vec<ChoiceOption>,
    /// `Some(description)` offers a decisions model a "nothing fits" option
    /// with that description; choosing it is [`Answer::NoneFits`]. Its key is
    /// `none`, prefixed with `_` until it collides with no option. Chat
    /// backends cannot say "nothing fits" and ignore it.
    pub none: Option<String>,
}

/// One classification question about one subject.
#[derive(Debug, Clone)]
pub struct ChoiceRequest {
    /// What is being judged, as labelled fields in display order
    /// (`("Task", title)`, `("Description", body)`, …). Empty values are the
    /// caller's to omit.
    pub subject: Vec<(String, String)>,
    /// What is asked about it.
    pub question: Question,
    /// The chat prompt: the question in the caller's own wording, asking for
    /// the answer shape [`ChatClassifier`] reads. Ignored by backends that are
    /// not chat-based — a decisions model reads [`Question::instructions`].
    pub chat_prompt: ChatPrompt,
}

/// A validated answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// One of the options: `key` is always an option's key and `confidence`
    /// always lies in `[0, 1]`.
    Chosen {
        /// The chosen option's key.
        key: String,
        /// How sure the backend is, `0.0..=1.0`. Callers compare it to their
        /// own threshold.
        confidence: f64,
        /// Why — surfaced by `--dry-run`, logs and notifications, never
        /// decided on.
        reason: String,
    },
    /// The backend judged that **none** of the options fits. Only a decisions
    /// backend asked a question with [`Question::none`] returns it.
    NoneFits {
        /// Why — surfaced like the `reason` of an [`Answer::Chosen`].
        reason: String,
    },
}

/// Asks a backend to choose one option.
pub trait ChoiceClassifier: Send + Sync {
    /// Ask once (transport retries aside) and return a validated answer.
    fn choose(
        &self,
        request: &ChoiceRequest,
    ) -> impl Future<Output = Result<Answer, ClassifyError>> + Send;

    /// Ask the backend whether it is there and accepts our credentials, with
    /// the cheapest request it will answer.
    ///
    /// A **liveness** question, not a correctness one, and never retried: it
    /// answers now or not at all. The default answers "alive" — right for a
    /// test double, and the only honest answer for a classifier with nothing
    /// to ask.
    fn probe(&self) -> impl Future<Output = Result<(), ClassifyError>> + Send {
        async { Ok(()) }
    }

    /// Drop every pooled connection, so the next call opens a fresh one
    /// (after the machine slept, F-111). A no-op for classifiers that hold no
    /// connections.
    fn reset_connections(&self) {}
}

/// Check a raw answer against the question's options and the `[0, 1]` range.
///
/// Every backend funnels through here, so "the answer names a real option" is
/// an invariant of [`Answer`] rather than something each caller has to
/// remember.
pub(crate) fn validated(
    key: String,
    confidence: f64,
    reason: String,
    question: &Question,
) -> Result<Answer, ClassifyError> {
    if !question.options.iter().any(|o| o.key == key) {
        return Err(ClassifyError::UnknownChoice(key));
    }
    if !(0.0..=1.0).contains(&confidence) {
        return Err(ClassifyError::InvalidResponse(format!(
            "`confidence` out of range [0,1]: {confidence}"
        )));
    }
    Ok(Answer::Chosen {
        key,
        confidence,
        reason,
    })
}
