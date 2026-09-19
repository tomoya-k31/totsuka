//! Repository classification: which of the configured repositories a piece of
//! work belongs to (F-11–F-14).
//!
//! Shared by the Orchestrator (`orchestrator-core`'s `repo_select`, for tasks
//! that arrive without a usable repo hint) and by task_source plugins that
//! resolve the repository themselves before submitting (`task-source-slack`).
//! Both used to carry their own HTTP client, retry loop, error shaping and
//! verdict parsing; this crate is the one copy.
//!
//! The boundary is the *question*, not the wire format: callers hand a
//! [`ClassifyRequest`] (what the work is + the candidates) to a
//! [`RepoClassifier`] and get a validated [`Classification`] back. Policy stays
//! with the caller — the confidence threshold, what to do on a low-confidence
//! or unusable answer, and whether to retry a bad answer at all.
//!
//! Layers:
//!
//! - [`HttpTransport`] — one authenticated JSON POST. The seam tests fake;
//!   [`ReqwestTransport`] is production.
//! - [`RetryPolicy`] — exponential backoff over retryable failures (§5.3).
//! - [`ChatClassifier`] — an OpenAI-compatible `/chat/completions` backend.

mod chat;
mod error;
mod retry;
mod transport;

use std::future::Future;

pub use chat::{ChatClassifier, ChatOutput, ChatPrompt, ChatSettings};
pub use error::ClassifyError;
pub use retry::RetryPolicy;
pub use transport::{ApiKey, HttpRequest, HttpTransport, ReqwestTransport, scrub_urls};

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

/// One classification question.
#[derive(Debug, Clone)]
pub struct ClassifyRequest {
    /// What is being classified, as labelled fields in display order
    /// (`("Task", title)`, `("Description", body)`, …). Empty values are the
    /// caller's to omit.
    pub subject: Vec<(String, String)>,
    /// The repositories to choose from. A verdict outside this list is
    /// rejected as [`ClassifyError::UnknownRepo`].
    pub candidates: Vec<Candidate>,
    /// A caller-rendered chat prompt (the Slack plugin's configurable
    /// templates). `None` uses the built-in prompt rendered from `subject`
    /// and `candidates`. Ignored by backends that are not chat-based.
    pub chat_prompt: Option<ChatPrompt>,
}

/// A validated verdict: `repo` is one of the candidates and `confidence` lies
/// in `[0, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// The chosen candidate's name.
    pub repo: String,
    /// How sure the backend is, `0.0..=1.0`. Callers compare it to their own
    /// threshold.
    pub confidence: f64,
    /// Why — surfaced by `--dry-run` and the logs, never decided on.
    pub reason: String,
}

/// Classifies work into one of a set of candidate repositories.
pub trait RepoClassifier: Send + Sync {
    /// Ask once (transport retries aside) and return a validated verdict.
    fn classify(
        &self,
        request: &ClassifyRequest,
    ) -> impl Future<Output = Result<Classification, ClassifyError>> + Send;

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

/// Check a raw verdict against the candidate set and the `[0, 1]` range.
///
/// Every backend funnels through here, so "the verdict names a real
/// candidate" is an invariant of [`Classification`] rather than something
/// each caller has to remember.
pub(crate) fn validated(
    repo: String,
    confidence: f64,
    reason: String,
    candidates: &[Candidate],
) -> Result<Classification, ClassifyError> {
    if !candidates.iter().any(|c| c.name == repo) {
        return Err(ClassifyError::UnknownRepo(repo));
    }
    if !(0.0..=1.0).contains(&confidence) {
        return Err(ClassifyError::InvalidResponse(format!(
            "`confidence` out of range [0,1]: {confidence}"
        )));
    }
    Ok(Classification {
        repo,
        confidence,
        reason,
    })
}
