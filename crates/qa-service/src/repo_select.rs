//! spec §8.4 step 3: apply confidence_threshold + on_low_confidence policy
//! to a classifier response.

use crate::classifier::{ClassifyResponse, RepoVerdict};
use crate::error::QaError;

#[derive(Debug, Clone, PartialEq)]
pub enum LowConfidencePolicy {
    DelegatedReaction,
    Refuse,
    UseTop1,
}

impl LowConfidencePolicy {
    pub fn parse(s: &str) -> Result<Self, QaError> {
        match s {
            "delegated_reaction" => Ok(Self::DelegatedReaction),
            "refuse" => Ok(Self::Refuse),
            "use_top1" => Ok(Self::UseTop1),
            other => Err(QaError::Internal(format!(
                "unknown on_low_confidence: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectOutcome {
    HighConfidence { repo: String, verdict: RepoVerdict },
    LowConfidenceDelegated { candidates: Vec<RepoVerdict> },
    LowConfidenceRefused,
    LowConfidenceUseTop1 { repo: String, verdict: RepoVerdict },
}

pub struct RepoSelector {
    threshold: f64,
    on_low: LowConfidencePolicy,
}

impl RepoSelector {
    pub fn from_cfg(threshold: f64, on_low: &str) -> Result<Self, QaError> {
        Ok(Self {
            threshold,
            on_low: LowConfidencePolicy::parse(on_low)?,
        })
    }

    pub fn decide(&self, response: &ClassifyResponse) -> SelectOutcome {
        let Some(top) = response.top_candidates.first() else {
            return SelectOutcome::LowConfidenceRefused;
        };
        if top.confidence >= self.threshold {
            return SelectOutcome::HighConfidence {
                repo: top.repo.clone(),
                verdict: top.clone(),
            };
        }
        match self.on_low {
            LowConfidencePolicy::DelegatedReaction => SelectOutcome::LowConfidenceDelegated {
                candidates: response.top_candidates.clone(),
            },
            LowConfidencePolicy::Refuse => SelectOutcome::LowConfidenceRefused,
            LowConfidencePolicy::UseTop1 => SelectOutcome::LowConfidenceUseTop1 {
                repo: top.repo.clone(),
                verdict: top.clone(),
            },
        }
    }
}
