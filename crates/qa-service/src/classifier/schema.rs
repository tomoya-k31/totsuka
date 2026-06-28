use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoCandidate {
    pub repo: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyRequest {
    pub question: String,
    pub thread_context: Option<String>,
    pub candidates: Vec<RepoCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoVerdict {
    pub repo: String,
    pub confidence: f64,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyResponse {
    pub top_candidates: Vec<RepoVerdict>,
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
}
