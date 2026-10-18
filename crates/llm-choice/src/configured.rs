//! [`ConfiguredClassifier`]: whichever backend the configuration named, as one
//! type.
//!
//! [`ChoiceClassifier`] uses `impl Future` returns, so it cannot be a trait
//! object; callers that pick the backend at runtime (from `[llm].api`) hold
//! this enum instead.

use crate::{
    Answer, ChatClassifier, ChoiceClassifier, ChoiceRequest, ClassifyError, DecisionsClassifier,
    HttpTransport,
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

impl<T: HttpTransport> ChoiceClassifier for ConfiguredClassifier<T> {
    async fn choose(&self, request: &ChoiceRequest) -> Result<Answer, ClassifyError> {
        match self {
            Self::Chat(c) => c.choose(request).await,
            Self::Decisions(d) => d.choose(request).await,
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
