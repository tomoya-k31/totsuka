//! LLM choice port: the classifier the engine asks questions of — today the
//! repository question (F-11–F-14), [`llm_choice::repo`].
//!
//! The trait and its types live in the shared `llm-choice` crate, because
//! task_source plugins classify with the same code and may only depend on
//! crates outside `orchestrator-core`. Re-exported here so the domain names its
//! ports in one place; the concrete backends are built in
//! [`adapters::llm`](crate::adapters::llm).

pub use llm_choice::repo::{Candidate, Classification, ClassifyRequest, classify};
pub use llm_choice::{Answer, ChoiceClassifier, ChoiceRequest, ClassifyError};
