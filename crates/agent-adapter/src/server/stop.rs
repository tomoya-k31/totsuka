//! `DELETE /v1/agents/:id` — stop agent and clean up worktree. Spec §8.2.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::repo::RepoKey;
use crate::server::AppState;

pub async fn stop(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.close(&aid).await {
        Ok(()) => {}
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            return Err(AdapterError::NotFound(id));
        }
        Err(e) => return Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
    let repo_hdr = headers
        .get("x-totsuka-repo")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let branch_hdr = headers
        .get("x-totsuka-branch")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    if let (Some(repo), Some(branch)) = (repo_hdr, branch_hdr) {
        if let Some(entry) = s.repos.resolve(&RepoKey::new(repo)) {
            // Best-effort worktree removal; failures get logged, not propagated.
            if let Err(e) = s.worktrees.remove(&entry, &branch).await {
                tracing::warn!(error=%e, "worktree remove failed during stop");
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
