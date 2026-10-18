//! Repository classifier port (F-11–F-14).
//!
//! The trait and its types live in the shared `llm-choice` crate, because
//! task_source plugins classify with the same code and may only depend on
//! crates outside `orchestrator-core`. Re-exported here so the domain names its
//! ports in one place; the concrete backends are built in
//! [`adapters::llm`](crate::adapters::llm). The repository question itself is
//! [`llm_choice::repo`].

pub use llm_choice::repo::{Candidate, Classification, ClassifyRequest};
pub use llm_choice::{Answer, ChoiceClassifier, ChoiceRequest, ClassifyError};
