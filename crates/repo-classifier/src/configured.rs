//! [`ConfiguredClassifier`]: whichever backend the configuration named, as one
//! type.
//!
//! [`RepoClassifier`] uses `impl Future` returns, so it cannot be a trait
//! object; callers that pick the backend at runtime (from `[llm].api`) hold
//! this enum instead.

use crate::{
    ChatClassifier, Classification, ClassifyError, ClassifyRequest, DecisionsClassifier,
    HttpTransport, RepoClassifier,
};

/// A chat or a decisions classifier.
#[derive(Debug)]
pub enum ConfiguredClassifier<T> {
    /// OpenAI-compatible `/chat/completions`.
    Chat(ChatClassifier<T>),
    /// A decisions model (TypeSafe Jev).
    Decisions(DecisionsClassifier<T>),
}

impl<T: HttpTransport> ConfiguredClassifier<T> {
    /// The URL requests go to — what `doctor` names when it reports on the key.
    pub fn endpoint(&self) -> String {
        match self {
            Self::Chat(c) => c.endpoint(),
            Self::Decisions(d) => d.settings().endpoint.clone(),
        }
    }
}

impl<T: HttpTransport> RepoClassifier for ConfiguredClassifier<T> {
    async fn classify(&self, request: &ClassifyRequest) -> Result<Classification, ClassifyError> {
        match self {
            Self::Chat(c) => c.classify(request).await,
            Self::Decisions(d) => d.classify(request).await,
        }
    }

    async fn probe(&self) -> Result<(), ClassifyError> {
        match self {
            Self::Chat(c) => c.probe().await,
            Self::Decisions(d) => d.probe().await,
        }
    }

    fn reset_connections(&self) {
        match self {
            Self::Chat(c) => c.reset_connections(),
            Self::Decisions(d) => d.reset_connections(),
        }
    }
}
