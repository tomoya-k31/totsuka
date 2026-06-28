use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct MockState {
    project_node_ids: HashMap<(String, u64), String>,
    project_pages: Vec<ProjectItemPage>,
    project_calls: usize,
    issues: HashMap<RepoSlug, Vec<IssueUpdate>>,
    prs: HashMap<RepoSlug, Vec<PrUpdate>>,
    releases: HashMap<RepoSlug, Vec<ReleaseUpdate>>,
}

pub struct MockGhClient {
    state: Mutex<MockState>,
}

impl Default for MockGhClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockGhClient {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MockState::default()),
        }
    }
    pub fn set_project_node_id(&self, owner: &str, number: u64, node_id: &str) {
        self.state
            .lock()
            .unwrap()
            .project_node_ids
            .insert((owner.into(), number), node_id.into());
    }
    pub fn set_project_items_pages(&self, pages: Vec<ProjectItemPage>) {
        let mut s = self.state.lock().unwrap();
        s.project_pages = pages;
        s.project_calls = 0;
    }
    pub fn set_issues(&self, repo: &RepoSlug, list: Vec<IssueUpdate>) {
        self.state.lock().unwrap().issues.insert(repo.clone(), list);
    }
    pub fn set_prs(&self, repo: &RepoSlug, list: Vec<PrUpdate>) {
        self.state.lock().unwrap().prs.insert(repo.clone(), list);
    }
    pub fn set_releases(&self, repo: &RepoSlug, list: Vec<ReleaseUpdate>) {
        self.state
            .lock()
            .unwrap()
            .releases
            .insert(repo.clone(), list);
    }
}

#[async_trait]
impl GhClient for MockGhClient {
    async fn resolve_project_node_id(
        &self,
        owner: &str,
        number: u64,
    ) -> Result<String, WatcherError> {
        self.state
            .lock()
            .unwrap()
            .project_node_ids
            .get(&(owner.into(), number))
            .cloned()
            .ok_or_else(|| {
                WatcherError::Internal(format!("mock has no project for {owner}/{number}"))
            })
    }

    async fn project_items_page(
        &self,
        _project_node_id: &str,
        _after: Option<&str>,
        _first: u32,
    ) -> Result<ProjectItemPage, WatcherError> {
        let mut s = self.state.lock().unwrap();
        if s.project_calls >= s.project_pages.len() {
            // Exhausted: return an empty terminal page so loops can converge.
            return Ok(ProjectItemPage {
                items: vec![],
                end_cursor: None,
                has_next: false,
            });
        }
        let p = s.project_pages[s.project_calls].clone();
        s.project_calls += 1;
        Ok(p)
    }

    async fn issues_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<IssueUpdate>, WatcherError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .issues
            .get(repo)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|u| u.updated_at > since)
            .collect())
    }
    async fn prs_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<PrUpdate>, WatcherError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .prs
            .get(repo)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|u| u.updated_at > since)
            .collect())
    }
    async fn releases_since(
        &self,
        repo: &RepoSlug,
        since: DateTime<Utc>,
    ) -> Result<Vec<ReleaseUpdate>, WatcherError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .releases
            .get(repo)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|u| u.published_at > since)
            .collect())
    }
}
