//! `GET /v1/agents/:id/output` — read agent pane snapshot. Spec §8.3.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::server::AppState;

#[derive(Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    pub since_revision: u64,
}

#[derive(Serialize)]
pub struct OutputResponse {
    pub revision: u64,
    pub text: String,
    pub is_newer: bool,
}

pub async fn output(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<OutputQuery>,
) -> Result<Json<OutputResponse>, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.read(&aid).await {
        Ok(snap) => Ok(Json(OutputResponse {
            revision: snap.revision,
            is_newer: snap.is_newer_than(q.since_revision),
            text: snap.text,
        })),
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            Err(AdapterError::NotFound(id))
        }
        Err(e) => Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
}
