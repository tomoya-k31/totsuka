//! `POST /v1/agents` — orchestrator-driven spawn. Spec §8.1.

use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::argv::check_argv;
use crate::error::AdapterError;
use crate::herdr::SpawnRequest;
use crate::repo::RepoKey;
use crate::server::AppState;

#[derive(Deserialize)]
pub struct SpawnBody {
    pub task_id: String,
    pub phase: String,
    pub attempt: i32,
    pub repo: String,
    pub branch: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct SpawnResponse {
    pub agent_id: String,
    pub terminal_id: String,
    pub worktree_path: String,
}

pub async fn spawn(
    State(s): State<AppState>,
    Json(body): Json<SpawnBody>,
) -> Result<(StatusCode, Json<SpawnResponse>), AdapterError> {
    if let Err(v) = check_argv(&body.argv) {
        return Err(AdapterError::ArgvSecretViolation(v.offending));
    }
    let repo = s
        .repos
        .resolve(&RepoKey::new(body.repo.clone()))
        .ok_or_else(|| AdapterError::RepoNotRegistered(body.repo.clone()))?;
    let worktree_path = s.worktrees.create(&repo, &body.branch).await?;
    let label = format!("totsuka:{}:{}:{}", body.task_id, body.phase, body.attempt);
    let res = s
        .herdr
        .start(SpawnRequest {
            cwd: worktree_path.to_string_lossy().into_owned(),
            argv: body.argv,
            env: body.env,
            label,
        })
        .await
        .map_err(|e| AdapterError::HerdrUnavailable(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(SpawnResponse {
            agent_id: res.agent_id.as_str().to_string(),
            terminal_id: res.terminal_id,
            worktree_path: worktree_path.to_string_lossy().into_owned(),
        }),
    ))
}
