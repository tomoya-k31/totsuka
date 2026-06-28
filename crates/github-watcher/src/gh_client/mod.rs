//! GitHub HTTPS contract. All shapes carry only what the watcher's
//! downstream pipeline (snapshot diff, PR linkage, event publishing) needs —
//! NOT a 1:1 GraphQL/REST mirror. Keep the surface narrow so MockGhClient is
//! easy to maintain.

use crate::error::WatcherError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

pub mod backoff;
pub mod graphql;
pub mod http;
pub mod mock;
pub mod rest;
pub use http::HttpGhClient;
pub use mock::MockGhClient;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl RepoSlug {
    pub fn parse(s: &str) -> Option<Self> {
        let (o, r) = s.split_once('/')?;
        if o.is_empty() || r.is_empty() || r.contains('/') {
            return None;
        }
        Some(Self {
            owner: o.into(),
            repo: r.into(),
        })
    }
}

impl std::fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectItemPage {
    pub items: Vec<ProjectItem>,
    pub end_cursor: Option<String>,
    pub has_next: bool,
}

#[derive(Debug, Clone)]
pub struct ProjectItem {
    pub id: String,
    pub status_display: Option<String>,
    pub repo: Option<RepoSlug>,
    pub content_number: Option<u64>,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct IssueUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub updated_at: DateTime<Utc>,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct PrUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub head_ref: String,
    pub body: Option<String>,
    pub merged: bool,
    pub merged_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ReleaseUpdate {
    pub node_id: String,
    pub repo: RepoSlug,
    pub tag: String,
    pub published_at: DateTime<Utc>,
}

#[async_trait]
pub trait GhClient: Send + Sync + 'static {
    async fn resolve_project_node_id(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<String, WatcherError>;

    async fn project_items_page(
        &self,
        project_node_id: &str,
        after: Option<&str>,
        first: u32,
    ) -> Result<ProjectItemPage, WatcherError>;

    async fn issues_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<IssueUpdate>, WatcherError>;

    async fn prs_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<PrUpdate>, WatcherError>;

    async fn releases_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<ReleaseUpdate>, WatcherError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_slug_round_trip() {
        let s = RepoSlug::parse("acme/widget").unwrap();
        assert_eq!(s.to_string(), "acme/widget");
        assert!(RepoSlug::parse("bad").is_none());
        assert!(RepoSlug::parse("a/b/c").is_none());
        assert!(RepoSlug::parse("/x").is_none());
    }
}
