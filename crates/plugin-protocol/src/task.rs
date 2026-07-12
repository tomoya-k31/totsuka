//! The normalized [`Task`] schema shared across task sources (F-01).
//!
//! Every task source plugin maps its native items (GitHub Issues, Notion pages,
//! …) onto this common shape so the Orchestrator is source-agnostic.

use serde::{Deserialize, Serialize};

/// A task in the normalized common schema (F-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Source's own stable identifier (Issue number, Notion page id).
    pub id: String,
    /// Source plugin instance name that produced this task (e.g. `github`).
    pub source: String,
    /// Short title.
    pub title: String,
    /// Full description / body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Repository hint from the source (e.g. the issue's repo, a Notion prop),
    /// used before falling back to LLM selection (F-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_hint: Option<String>,
    /// Labels/tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Priority; higher runs first.
    #[serde(default)]
    pub priority: i64,
    /// Source-side status (column/property value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// URL to the task in the source system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Assignee, if any (used for ingest gating, F-08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_omits_empty_optionals() {
        let task = Task {
            id: "42".into(),
            source: "github".into(),
            title: "Fix bug".into(),
            body: None,
            repo_hint: Some("totsuka".into()),
            labels: vec![],
            priority: 0,
            status: Some("実装待ち".into()),
            url: None,
            assignee: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("body"), "empty optionals omitted");
        assert!(!json.contains("labels"));
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back, task);
    }
}
