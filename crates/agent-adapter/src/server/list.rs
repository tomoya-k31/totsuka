//! `GET /v1/agents` — list live agents. qa-service's boot recovery
//! reconciles `qa_thread_agent` mappings against this, and its readyz
//! adapter probe calls it; both were dead letters while the route 405'd.

use axum::{extract::State, Json};
use serde::Serialize;

use crate::error::AdapterError;
use crate::server::AppState;

#[derive(Serialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub terminal_id: String,
    pub label: String,
}

pub async fn list(State(s): State<AppState>) -> Result<Json<Vec<AgentSummary>>, AdapterError> {
    let items = s
        .herdr
        .list()
        .await
        .map_err(|e| AdapterError::HerdrUnavailable(e.to_string()))?;
    Ok(Json(
        items
            .into_iter()
            .map(|i| AgentSummary {
                // Agent ids ARE herdr terminal ids in this design (see
                // WireHerdr::start); expose both fields for the consumer.
                agent_id: i.agent_id.as_str().to_string(),
                terminal_id: i.agent_id.as_str().to_string(),
                label: i.label,
            })
            .collect(),
    ))
}
