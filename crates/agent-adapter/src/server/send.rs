//! `POST /v1/agents/:id/messages` — send message to agent. Spec §8.2.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use crate::error::AdapterError;
use crate::herdr::{AgentId, HerdrError};
use crate::server::AppState;

#[derive(Deserialize)]
pub struct SendBody {
    pub text: String,
}

pub async fn send(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> Result<StatusCode, AdapterError> {
    let aid = AgentId::new(id.clone());
    match s.herdr.send(&aid, &body.text).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(HerdrError::Remote { code, .. }) if code == "not_found" => {
            Err(AdapterError::NotFound(id))
        }
        Err(e) => Err(AdapterError::HerdrUnavailable(e.to_string())),
    }
}
