//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/github.toml` as JSON with secrets already expanded (F-64/F-65).

use std::collections::HashMap;

use serde::Deserialize;

/// Whether the project owner is a user or an organization (GraphQL requires
/// choosing the right root field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OwnerType {
    /// A user account (`user(login:)`).
    #[default]
    User,
    /// An organization (`organization(login:)`).
    Organization,
}

impl OwnerType {
    /// The GraphQL root field selecting this owner's `projectV2`.
    pub fn graphql_root(self) -> &'static str {
        match self {
            OwnerType::User => "user",
            OwnerType::Organization => "organization",
        }
    }
}

/// GitHub task-source settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// API token (resolved by the orchestrator, F-65). Never touched by us
    /// beyond sending it as a bearer token.
    pub token: String,
    /// Project owner login (user or org).
    pub owner: String,
    /// Whether `owner` is a user or an organization.
    #[serde(default)]
    pub owner_type: OwnerType,
    /// ProjectsV2 number under `owner`.
    pub project_number: i64,
    /// SingleSelect field name holding the status column (F-02).
    #[serde(default = "default_status_field")]
    pub status_field: String,
    /// The operator's own login, used to detect self-assigned tasks (F-08).
    pub github_login: String,
    /// Status names treated as "in progress" and therefore excluded from
    /// ingest (F-08).
    #[serde(default)]
    pub in_progress_statuses: Vec<String>,
    /// Maps an orchestrator-side status name to the project's SingleSelect
    /// option name for `task/update_status` (F-84). Identity when absent.
    #[serde(default)]
    pub status_map: HashMap<String, String>,
    /// Restricts ingest to issues in these repositories (by name). Empty = any
    /// repo in the project.
    #[serde(default)]
    pub repos: Vec<String>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// GraphQL endpoint (overridable for GitHub Enterprise / tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl GithubConfig {
    /// Resolve the project option name for an orchestrator status via
    /// [`status_map`](Self::status_map), falling back to the name itself.
    pub fn map_status<'a>(&'a self, status: &'a str) -> &'a str {
        self.status_map.get(status).map_or(status, String::as_str)
    }

    /// Whether a task with these `assignees` may be ingested by this operator
    /// (F-08): unassigned, or `github_login` is among the assignees. A task
    /// assigned only to other people is skipped. Matching is over the whole
    /// list, so ingest never depends on GitHub's assignee ordering.
    pub fn assignable_to_me(&self, assignees: &[&str]) -> bool {
        assignees.is_empty()
            || assignees
                .iter()
                .any(|login| login.eq_ignore_ascii_case(&self.github_login))
    }

    /// Whether `status` is an "in progress" column excluded from ingest (F-08).
    pub fn is_in_progress(&self, status: &str) -> bool {
        self.in_progress_statuses.iter().any(|s| s == status)
    }

    /// Whether `repo` passes the optional repository filter (empty = any).
    pub fn repo_allowed(&self, repo: &str) -> bool {
        self.repos.is_empty() || self.repos.iter().any(|r| r == repo)
    }
}

fn default_status_field() -> String {
    "Status".to_string()
}
fn default_source_name() -> String {
    "github".to_string()
}
fn default_api_url() -> String {
    "https://api.github.com/graphql".to_string()
}
fn default_max_retries() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> GithubConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(serde_json::json!({
            "token": "t", "owner": "me", "project_number": 1, "github_login": "me"
        }));
        assert_eq!(cfg.status_field, "Status");
        assert_eq!(cfg.source_name, "github");
        assert_eq!(cfg.api_url, "https://api.github.com/graphql");
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.owner_type, OwnerType::User);
        assert!(cfg.repos.is_empty());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = serde_json::from_value::<GithubConfig>(serde_json::json!({
            "token": "t", "owner": "me", "project_number": 1, "github_login": "me",
            "typo_field": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn ingest_gating_helpers() {
        let cfg = parse(serde_json::json!({
            "token": "t", "owner": "me", "project_number": 1, "github_login": "Me",
            "in_progress_statuses": ["In Progress"], "repos": ["totsuka"]
        }));
        // Self-detection is case-insensitive; others are excluded; and I count
        // as assigned even when I am not the first assignee.
        assert!(cfg.assignable_to_me(&[]));
        assert!(cfg.assignable_to_me(&["me"]));
        assert!(cfg.assignable_to_me(&["another-dev", "me"]));
        assert!(!cfg.assignable_to_me(&["someone-else"]));
        assert!(!cfg.assignable_to_me(&["a", "b"]));
        assert!(cfg.is_in_progress("In Progress"));
        assert!(!cfg.is_in_progress("Todo"));
        assert!(cfg.repo_allowed("totsuka"));
        assert!(!cfg.repo_allowed("other"));
    }

    #[test]
    fn status_map_falls_back_to_identity() {
        let cfg = parse(serde_json::json!({
            "token": "t", "owner": "me", "project_number": 1, "github_login": "me",
            "status_map": { "レビュー待ち": "In Review" }
        }));
        assert_eq!(cfg.map_status("レビュー待ち"), "In Review");
        assert_eq!(cfg.map_status("実装待ち"), "実装待ち");
    }
}
