//! Per-poller modules + a shared RepoTracker that the project poller updates
//! as it observes ProjectsV2 items, and the issues/PRs/releases pollers read
//! to know which repos to scan.

use crate::gh_client::RepoSlug;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod issues;
pub mod project;
pub mod prs;
pub mod releases;

#[derive(Default, Clone)]
pub struct RepoTracker {
    inner: Arc<RwLock<HashSet<RepoSlug>>>,
}

impl RepoTracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn insert(&self, repo: RepoSlug) {
        self.inner.write().await.insert(repo);
    }
    pub async fn snapshot(&self) -> Vec<RepoSlug> {
        self.inner.read().await.iter().cloned().collect()
    }
    pub async fn known(&self, repo: &RepoSlug) -> bool {
        self.inner.read().await.contains(repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tracker_collects_and_dedupes() {
        let t = RepoTracker::new();
        t.insert(RepoSlug::parse("a/x").unwrap()).await;
        t.insert(RepoSlug::parse("a/x").unwrap()).await;
        t.insert(RepoSlug::parse("a/y").unwrap()).await;
        let mut got = t.snapshot().await;
        got.sort_by(|a, b| a.repo.cmp(&b.repo));
        assert_eq!(got.len(), 2);
        assert!(t.known(&RepoSlug::parse("a/x").unwrap()).await);
        assert!(!t.known(&RepoSlug::parse("a/z").unwrap()).await);
    }
}
