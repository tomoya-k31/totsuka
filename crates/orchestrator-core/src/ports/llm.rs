//! Repository classifier port (F-11–F-14).
//!
//! The trait and its types live in the shared `repo-classifier` crate, because
//! task_source plugins classify with the same code and may only depend on
//! crates outside `orchestrator-core`. Re-exported here so the domain names its
//! ports in one place; the concrete backends are built in
//! [`adapters::llm`](crate::adapters::llm).

pub use repo_classifier::{
    Candidate, Classification, ClassifyError, ClassifyRequest, RepoClassifier,
};
